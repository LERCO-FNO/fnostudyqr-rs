use dicom_core::{DataElement, PrimitiveValue, VR, dicom_value};
use dicom_dictionary_std::{tags, uids};
use dicom_dump::DumpOptions;
use dicom_encoding::{TransferSyntax, TransferSyntaxIndex};
use dicom_object::{InMemDicomObject, StandardDataDictionary};
use dicom_transfer_syntax_registry::{TransferSyntaxRegistry, entries};
use dicom_ul::{
    ClientAssociation, ClientAssociationOptions, Pdu,
    pdu::{PDataValue, PDataValueType},
};
use snafu::{OptionExt, ResultExt};
use std::io::{Read, stderr};
use tracing::{debug, error, info, warn};

use crate::{
    DumpOutputSnafu, Error, InformationLevel, InitScuSnafu, ReadCommandSnafu, RequestMode,
};

#[derive(Clone, Copy)]
pub enum Mode {
    Find,
    Move,
}

impl From<&RequestMode> for Mode {
    fn from(value: &RequestMode) -> Self {
        match value {
            RequestMode::Find { .. } => Mode::Find,
            RequestMode::Move { .. } => Mode::Move,
        }
    }
}

pub struct ScuClient {
    assoc: ClientAssociation<std::net::TcpStream>,
    abstract_syntax: String,
    pc_id: u8,
    ts: &'static TransferSyntax,
    verbose: bool,
}

impl ScuClient {
    pub fn new(
        mode: Mode,
        addr: String,
        information_level: InformationLevel,
        calling_ae_title: String,
        called_ae_title: Option<String>,
        verbose: bool,
    ) -> Result<Self, Error> {
        let abstract_syntax = match (mode, information_level) {
            (Mode::Find, InformationLevel::Patient) => {
                uids::PATIENT_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_FIND
            }
            (Mode::Find, InformationLevel::Study | InformationLevel::Series) => {
                uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_FIND
            }
            (Mode::Move, InformationLevel::Patient) => {
                uids::PATIENT_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_MOVE
            }
            (Mode::Move, InformationLevel::Study | InformationLevel::Series) => {
                uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_MOVE
            }
        }
        .to_string();

        if verbose {
            info!("Establishing association with '{}'...", &addr);
        }

        let mut scu_opt = ClientAssociationOptions::new()
            .with_abstract_syntax(&abstract_syntax)
            .calling_ae_title(calling_ae_title)
            .max_pdu_length(16378);

        if let Some(called_ae_title) = called_ae_title {
            scu_opt = scu_opt.called_ae_title(called_ae_title);
        }
        let assoc = scu_opt.establish_with(&addr).context(InitScuSnafu)?;

        if verbose {
            info!("Association established");
        }

        let (pc_id, ts_uid) = match assoc.presentation_contexts().first() {
            Some(pc) => (pc.id, pc.transfer_syntax.clone()),
            None => {
                error!("Could not choose a presentation context");
                let _ = assoc.abort();
                info!("Association aborted");
                return Err(Error::NoPresentationContext);
            }
        };

        let ts = match TransferSyntaxRegistry.get(&ts_uid) {
            Some(ts) => ts.to_owned(),
            None => {
                error!("Poorly negotiated transfer syntax");
                let _ = assoc.abort();
                info!("Association aborted");
                return Err(Error::UnsupportedTransferSyntax);
            }
        };

        if verbose {
            debug!("Transfer syntax: {}", ts.name());
        }

        Ok(Self {
            assoc,
            abstract_syntax,
            pc_id,
            ts,
            verbose,
        })
    }

    pub fn find_study(
        &mut self,
        ds_queries: Vec<InMemDicomObject>,
        // out_response_file: PathBuf,
    ) -> Result<Vec<InMemDicomObject>, Error> {
        let mut responses: Vec<InMemDicomObject> = Vec::new();

        let ds_len = ds_queries.len() as u16;
        for (ds, index) in ds_queries.into_iter().zip(1..=ds_len) {
            let cmd = find_req_command(&self.abstract_syntax, index);
            let mut cmd_data = Vec::with_capacity(128);
            cmd.write_dataset_with_ts(&mut cmd_data, &entries::IMPLICIT_VR_LITTLE_ENDIAN.erased())
                .whatever_context("Failed to write command")?;

            let mut iod_data = Vec::with_capacity(128);
            ds.write_dataset_with_ts(&mut iod_data, self.ts)
                .whatever_context("Failed to write identifier to dataset")?;

            let nbytes = cmd_data.len() + iod_data.len();

            if self.verbose {
                debug!("Sending query ({nbytes} B)...");
            }

            let pdu = Pdu::PData {
                data: vec![PDataValue {
                    presentation_context_id: self.pc_id,
                    value_type: PDataValueType::Command,
                    is_last: true,
                    data: cmd_data,
                }],
            };
            self.assoc
                .send(&pdu)
                .whatever_context("Could not send C-FIND command")?;

            let pdu = Pdu::PData {
                data: vec![PDataValue {
                    presentation_context_id: self.pc_id,
                    value_type: PDataValueType::Data,
                    is_last: true,
                    data: iod_data,
                }],
            };
            self.assoc
                .send(&pdu)
                .whatever_context("Could not send C-FIND request")?;

            if self.verbose {
                debug!("Awaiting response...");
            }

            let mut i = 0;
            loop {
                let rsp_pdu = self
                    .assoc
                    .receive()
                    .whatever_context("Failed to receive response from remote node")?;

                match rsp_pdu {
                    Pdu::PData { data } => {
                        if data.is_empty() {
                            error!("Empty PData response");
                            break;
                        } else if ![1, 2].contains(&data.len()) {
                            warn!(
                                "Unexpected number of PDataValue parts: {} (allowed 1 or 2)",
                                data.len()
                            );
                            break;
                        }

                        let data_value = &data[0];
                        let cmd_obj = InMemDicomObject::read_dataset_with_ts(
                            &data_value.data[..],
                            &entries::IMPLICIT_VR_LITTLE_ENDIAN.erased(),
                        )
                        .context(ReadCommandSnafu)?;

                        if self.verbose {
                            eprintln!("Match #{i} response command:");
                            DumpOptions::new()
                                .dump_object_to(stderr(), &cmd_obj)
                                .context(DumpOutputSnafu)?;
                        }

                        let status = cmd_obj
                            .get(tags::STATUS)
                            .whatever_context("Status code from response is missing")?
                            .to_int::<u16>()
                            .whatever_context("Failed to read status code")?;
                        if status == 0 {
                            if self.verbose {
                                debug!("Matching is complete");
                            }

                            if i == 0 {
                                info!("No results matching query")
                            }
                            break;
                        } else if status == 0xFF00 || status == 0xFF01 {
                            if self.verbose {
                                debug!("Operation pending: 0x{status:X}");
                            }

                            let dcm_obj = if let Some(second_pdata) = data.get(1) {
                                InMemDicomObject::read_dataset_with_ts(
                                    second_pdata.data.as_slice(),
                                    self.ts,
                                )
                                .whatever_context("Could not read response data set")?
                            } else {
                                let mut rsp = self.assoc.receive_pdata();
                                let mut response_data = Vec::new();
                                rsp.read_to_end(&mut response_data)
                                    .whatever_context("Failed to read response data")?;
                                InMemDicomObject::read_dataset_with_ts(&response_data[..], self.ts)
                                    .whatever_context("Could not read response data set")?
                            };

                            println!(
                                "------------------------ Match #{i} ------------------------"
                            );
                            DumpOptions::new()
                                .dump_object(&dcm_obj)
                                .context(DumpOutputSnafu)?;

                            let status = dcm_obj
                                .get(tags::STATUS)
                                .and_then(|el| el.to_int::<u16>().ok());
                            responses.push(dcm_obj);

                            // check dicom status in response data
                            if status == Some(0) {
                                if self.verbose {
                                    debug!("Matching is complete");
                                }
                                break;
                            }
                            i += 1;
                        } else {
                            warn!("Operation failed (status code {status:X})");
                            break;
                        }
                    }
                    pdu @ Pdu::Unknown { .. }
                    | pdu @ Pdu::AssociationRQ { .. }
                    | pdu @ Pdu::AssociationAC { .. }
                    | pdu @ Pdu::AssociationRJ { .. }
                    | pdu @ Pdu::ReleaseRQ
                    | pdu @ Pdu::ReleaseRP
                    | pdu @ Pdu::AbortRQ { .. } => {
                        error!("Unexpected SCP response: {:?}", pdu);
                        return Err(Error::UnexpctedSCPResponse);
                    }
                }
            }
        }

        if responses.is_empty() {
            return Err(Error::NoResponsesToWrite);
        }

        Ok(responses)
    }

    pub fn move_study(
        &mut self,
        ds_queries: Vec<InMemDicomObject>,
        move_destination: &str,
    ) -> Result<Vec<InMemDicomObject>, Error> {
        let mut responses: Vec<InMemDicomObject> = Vec::new();

        let ds_len = ds_queries.len() as u16;
        for (ds, index) in ds_queries.into_iter().zip(1..=ds_len) {
            let cmd = move_req_command(&self.abstract_syntax, move_destination, index);
            let mut cmd_data = Vec::with_capacity(128);
            cmd.write_dataset_with_ts(&mut cmd_data, &entries::IMPLICIT_VR_LITTLE_ENDIAN.erased())
                .whatever_context("Failed to write command")?;

            let mut iod_data = Vec::with_capacity(128);
            ds.write_dataset_with_ts(&mut iod_data, self.ts)
                .whatever_context("Failed to write identifier dataset")?;

            let nbytes = cmd_data.len() + iod_data.len();

            if self.verbose {
                debug!("Sending query ({nbytes} B)...");
            }

            let pdu = Pdu::PData {
                data: vec![PDataValue {
                    presentation_context_id: self.pc_id,
                    value_type: PDataValueType::Command,
                    is_last: true,
                    data: cmd_data,
                }],
            };
            self.assoc
                .send(&pdu)
                .whatever_context("Could not send C-MOVE command")?;

            let pdu = Pdu::PData {
                data: vec![PDataValue {
                    presentation_context_id: self.pc_id,
                    value_type: PDataValueType::Data,
                    is_last: true,
                    data: iod_data,
                }],
            };
            self.assoc
                .send(&pdu)
                .whatever_context("Could not send C-MOVE request")?;

            if self.verbose {
                debug!("Awaiting response...");
            }

            let mut i = 0;
            // let mut success = false;
            loop {
                let rsp_pdu = self
                    .assoc
                    .receive()
                    .whatever_context("Failed to receive response from remote node")?;

                match rsp_pdu {
                    Pdu::PData { data } => {
                        if data.is_empty() {
                            error!("Empty PData response");
                            break;
                        } else if ![1, 2].contains(&data.len()) {
                            warn!(
                                "Unexpected number of PDataValue parts: {} (allowed 1 or 2)",
                                data.len()
                            );
                            break;
                        }

                        let data_value = &data[0];
                        let cmd_obj = InMemDicomObject::read_dataset_with_ts(
                            &data_value.data[..],
                            &entries::IMPLICIT_VR_LITTLE_ENDIAN.erased(),
                        )
                        .context(ReadCommandSnafu)?;

                        if self.verbose {
                            eprint!("Match #{i} response command:");
                            DumpOptions::new()
                                .dump_object_to(stderr(), &cmd_obj)
                                .context(DumpOutputSnafu)?;
                        }

                        let status = cmd_obj
                            .get(tags::STATUS)
                            .whatever_context("Status code from response is missing")?
                            .to_int::<u16>()
                            .whatever_context("Failed to read status code")?;

                        if status == 0 {
                            if self.verbose {
                                debug!("Matching is complete");
                            }
                            if i == 0 {
                                info!("No results matching query");
                            }
                            // success = true;
                            break;
                        } else if status == 0xFF00 || status == 0xFF01 {
                            if self.verbose {
                                debug!("Operation pending: 0x{status:X}");
                            }

                            let dcm_obj = if let Some(second_pdata) = data.get(1) {
                                InMemDicomObject::read_dataset_with_ts(
                                    second_pdata.data.as_slice(),
                                    self.ts,
                                )
                                .whatever_context("Could not read response data set")?
                            } else {
                                let mut rsp = self.assoc.receive_pdata();
                                let mut response_data = Vec::new();
                                rsp.read_to_end(&mut response_data)
                                    .whatever_context("Failed to read response data")?;
                                InMemDicomObject::read_dataset_with_ts(&response_data[..], self.ts)
                                    .whatever_context("Could not read response data set")?
                            };

                            /*println!(
                                "------------------------ Match #{i} ------------------------"
                            );
                            DumpOptions::new()
                                .dump_object(&dcm_obj)
                                .context(DumpOutputSnafu)?;*/

                            let status = dcm_obj
                                .get(tags::STATUS)
                                .and_then(|el| el.to_int::<u16>().ok());
                            responses.push(dcm_obj);

                            if status == Some(0) {
                                if self.verbose {
                                    debug!("Matching is complete");
                                }
                                break;
                            }
                            i += 1;
                        } else {
                            let msg = match status {
                                0xa701 => "Out of resources (number of matches)",
                                0xa702 => "Out of resources (sub-operations)",
                                0x0122 => "SOP class not supported",
                                0xa801 => "Move destination unknown",
                                0xa900 => "Identifier does not match SOP class in C-MOVE response",
                                0xc000 => "Unable to process C-MOVE response",
                                0xfe00 => "Sub-operations terminated due to cancel indication",
                                0xb000 => "Sub-operations complete with one or more failures",
                                _ => "Unknown status code",
                            };
                            warn!("Operation failed (status code {status:x}) {msg}");
                            break;
                        }
                    }
                    pdu @ Pdu::Unknown { .. }
                    | pdu @ Pdu::AssociationRQ { .. }
                    | pdu @ Pdu::AssociationAC { .. }
                    | pdu @ Pdu::AssociationRJ { .. }
                    | pdu @ Pdu::ReleaseRQ
                    | pdu @ Pdu::ReleaseRP
                    | pdu @ Pdu::AbortRQ { .. } => {
                        error!("Unexpected SCP response: {:?}", pdu);
                        return Err(Error::UnexpctedSCPResponse);
                    }
                }
            }
        }

        if responses.is_empty() {
            return Err(Error::NoResponsesToWrite);
        }

        Ok(responses)
    }

    pub fn release_assoc(self) {
        let _ = self.assoc.release();
        if self.verbose {
            info!("Association released");
        }
    }
}

fn find_req_command(
    sop_class_uid: &str,
    message_id: u16,
) -> InMemDicomObject<StandardDataDictionary> {
    InMemDicomObject::command_from_element_iter([
        // SOP Class UID
        DataElement::new(
            tags::AFFECTED_SOP_CLASS_UID,
            VR::UI,
            PrimitiveValue::from(sop_class_uid),
        ),
        // command field
        DataElement::new(
            tags::COMMAND_FIELD,
            VR::US,
            // 0020H: C-FIND-RQ message
            dicom_value!(U16, [0x0020]),
        ),
        // message ID
        DataElement::new(tags::MESSAGE_ID, VR::US, dicom_value!(U16, [message_id])),
        //priority
        DataElement::new(
            tags::PRIORITY,
            VR::US,
            // medium
            dicom_value!(U16, [0x0000]),
        ),
        // data set type
        DataElement::new(
            tags::COMMAND_DATA_SET_TYPE,
            VR::US,
            dicom_value!(U16, [0x0001]),
        ),
    ])
}

fn move_req_command(
    sop_class_uid: &str,
    move_destination: &str,
    message_id: u16,
) -> InMemDicomObject<StandardDataDictionary> {
    InMemDicomObject::command_from_element_iter([
        // SOP Class UID
        DataElement::new(
            tags::AFFECTED_SOP_CLASS_UID,
            VR::UI,
            PrimitiveValue::from(sop_class_uid),
        ),
        // command field
        DataElement::new(
            tags::COMMAND_FIELD,
            VR::US,
            // 0021H: C-MOVE-RQ message  --> suggestion to create constants for these
            dicom_value!(U16, [0x0021]),
        ),
        // message ID
        DataElement::new(tags::MESSAGE_ID, VR::US, dicom_value!(U16, [message_id])),
        //priority
        DataElement::new(
            tags::PRIORITY,
            VR::US,
            // medium
            dicom_value!(U16, [0x0000]),
        ),
        // data set type
        DataElement::new(
            tags::COMMAND_DATA_SET_TYPE,
            VR::US,
            dicom_value!(U16, [0x0001]),
        ),
        // data set type
        DataElement::new(
            tags::MOVE_DESTINATION,
            VR::AE,
            PrimitiveValue::from(move_destination),
        ),
    ])
}
