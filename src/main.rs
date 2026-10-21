use clap::{Parser, Subcommand, ValueEnum};
use snafu::{Report, Whatever, prelude::*};
use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::str::FromStr;
use tracing::{error, info, warn};
use tracing_subscriber::fmt::time::ChronoLocal;

mod client;
mod deserialize;
mod query;
mod serialize;
mod store_async;
mod utils;

use crate::client::ScuClient;
use crate::query::*;
use crate::serialize::write_responses;
use crate::store_async::run_store_async;
use crate::utils::validate_response_filepath;

/// DICOM C-FIND/C-MOVE application
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Socket address to SCP, ex: "<AET>@127.0.0.1:1045". Called <AET> prefix is optional,
    /// otherwise set with --called-ae-title.
    addr: String,
    /// Input file with study identifiers. Requires non-empty StudyInstanceUID or PatientID with StudyDate.
    #[arg(short = 'i', long, value_name = "PATH")]
    in_study_file: Option<PathBuf>,
    /// Additional sequence of tags
    #[arg(short = 'q', long)]
    query_tag: Vec<String>,
    /// Calling AE title
    #[arg(short = 't', long = "calling-ae-title", required = true)]
    calling_ae_title: String,
    /// Called AE title
    #[arg(short = 'c', long = "called-ae-title")]
    called_ae_title: Option<String>,
    /// Information model for QueryRetrieveLevel tag
    #[arg(short = 'l', long, default_value = "study")]
    information_level: InformationLevel,
    /// Verbose mode
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Request mode
    #[command(subcommand)]
    request_mode: RequestMode,
}

#[derive(Debug, Clone, ValueEnum)]
enum InformationLevel {
    /// Use patient level information model
    Patient,
    /// Use study level information model
    Study,
    /// Use series level information model
    Series,
}

#[derive(Subcommand, Debug)]
enum RequestMode {
    Find {
        /// Path to file/directory to write response tags
        #[arg(short = 'f', long, value_name = "PATH", value_parser = validate_response_filepath)]
        out_response_path: Option<PathBuf>,
    },
    Move {
        /// C-MOVE destination AE title. Defaults to --calling-ae-title
        #[arg(long = "move-destination")]
        move_destination: Option<String>,
        /// Store port to listen on
        #[arg(short = 'p', long)]
        store_port: u16,
        /// Output directory for incoming objects
        #[arg(short = 'o', long, value_name = "PATH", default_value = "./output")]
        output_dir: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum IOFileFormat {
    Csv,
    Json,
}

impl FromStr for IOFileFormat {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "csv" => Ok(IOFileFormat::Csv),
            "json" => Ok(IOFileFormat::Json),
            _ => Err(format!("Unsupported file extension {value}")),
        }
    }
}

#[derive(Debug, Snafu)]
enum Error {
    /// Could not initialize SCU
    InitScu {
        source: dicom_ul::association::Error,
    },

    /// Could not construct DICOM command
    _CreateCommand {
        source: dicom_object::ReadError,
    },

    /// Could not read DICOM command
    ReadCommand {
        source: dicom_object::ReadError,
    },

    /// Could not dump DICOM output
    DumpOutput {
        source: std::io::Error,
    },
    #[snafu(display("Could not create datasets from file: {reason}"))]
    IOFileFormatExtension {
        reason: String,
    },
    /// Could not desrialize DICOM objects from input file
    #[snafu(display("Failed deserializing datasets from file `{}`", path.display()))]
    DeserDatasetsFromFile {
        source: deserialize::DeserError,
        path: PathBuf,
    },

    #[snafu(display("Failed opening file at `{}`", path.display()))]
    OpenFile {
        source: std::io::Error,
        path: PathBuf,
    },

    NoPresentationContext,

    UnsupportedTransferSyntax,

    UnexpectedSCPResponse,

    NoResponsesToWrite,

    #[snafu(display("No response returned"))]
    NoResponseToWrite,

    #[snafu(display("Could not create output file `{}`, {source}", path.display()))]
    CreateOutputFile {
        source: std::io::Error,
        path: PathBuf,
    },
    #[snafu(display("{source}"))]
    SerializeJson {
        source: serde_json::Error,
    },
    #[snafu(display("{source}"))]
    SerializeCsv {
        source: csv::Error,
    },
    #[snafu(display(
        "Query #{}: absent/empty required identifier(s), set non-empty either PatientID (0010,0020) with StudyDate (0008,0020) or StudyInstanceUID (0008,000D)",
        index + 1
    ))]
    NoRequiredIdentifier {
        index: usize,
    },
    #[snafu(whatever, display("{}", message))]
    Other {
        message: String,
        #[snafu(source(from(Box<dyn std::error::Error + 'static>, Some)))]
        source: Option<Box<dyn std::error::Error + 'static>>,
    },
}

fn main() {
    run().unwrap_or_else(|err| {
        error!("{}", snafu::Report::from_error(err));
        std::process::exit(-2);
    });
}

fn run() -> Result<(), Error> {
    let Args {
        request_mode,
        addr,
        in_study_file,
        query_tag,
        calling_ae_title,
        called_ae_title,
        information_level,
        verbose,
    } = Args::parse();

    tracing::subscriber::set_global_default(
        tracing_subscriber::FmtSubscriber::builder()
            .with_timer(ChronoLocal::new("%Y-%m-%d %H:%M:%S".to_string()))
            .with_max_level(if verbose {
                tracing::Level::DEBUG
            } else {
                tracing::Level::INFO
            })
            .finish(),
    )
    .unwrap_or_else(|e| {
        error!("{}", snafu::Report::from_error(e));
    });

    let query_tags = parse_query_tags(query_tag, &information_level)
        .whatever_context("Failed to parse query tags from command line")?;
    let ds_queries = build_queries(in_study_file, query_tags, verbose)?;

    let mut client = ScuClient::new(
        (&request_mode).into(),
        addr,
        information_level,
        calling_ae_title.clone(),
        called_ae_title,
        verbose,
    )?;

    info!("Requesting {} query(ies)", ds_queries.len());
    let _query_result = match request_mode {
        RequestMode::Find {
            out_response_path: out_response_filepath,
        } => {
            let res = client.find_study(&ds_queries);
            let responses = match res {
                // case 1: Vec<FindResult> is empty OR Vec<FindResult> not empty AND all FindResult empty -> no matches
                Ok(responses)
                    if responses.is_empty() || responses.iter().all(|r| r.matches.is_empty()) =>
                {
                    info!("No matches received, nothing to write");
                    return Ok(());
                }
                // case 2: Vec<FindResult> not empty and any FindResult not empty
                Ok(responses) => responses,
                Err(err) => {
                    error!("{err}");
                    return Ok(());
                }
            };

            if let Some(out_file_path) = out_response_filepath {
                write_responses(out_file_path, &ds_queries, &responses)?;
            } else {
                warn!(
                    "Received {} response(s) but no output response path was given; responses were not written",
                    responses.len()
                );
            };
            Ok(())
        }
        RequestMode::Move {
            move_destination,
            store_port,
            output_dir,
        } => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let store_args = StoreScpArgs {
                calling_ae_title: calling_ae_title.clone(),
                output_dir,
                store_port,
                verbose,
            };

            let handle = runtime.spawn(async move {
                let _ = run_async(store_args).await.unwrap_or_else(|err| {
                    error!("{:?}", Report::from_error(err));
                    std::process::exit(-2);
                });
            });

            let move_destination = move_destination.unwrap_or(calling_ae_title);
            let res = client.move_study(&ds_queries.queries, &move_destination);
            handle.abort();
            res
        }
    };

    // BUG: putting wrong AE title doesn't release/abort association

    client.release_assoc();
    Ok(())
}

fn parse_query_tags(
    query_tags: Vec<String>,
    level: &InformationLevel,
) -> Result<Vec<TermQuery>, Whatever> {
    let mut tags = query_tags
        .iter()
        .map(|t| t.parse::<TermQuery>())
        .collect::<Result<HashSet<TermQuery>, _>>()
        .whatever_context("Could not parse query tags")?;

    let query_retrieve_level: TermQuery = match level {
        InformationLevel::Patient => "0008,0052=PATIENT",
        InformationLevel::Study => "0008,0052=STUDY",
        InformationLevel::Series => "0008,0052=SERIES",
    }
    .parse()?;

    if let Some(old) = tags.replace(query_retrieve_level) {
        warn!("Do not add DICOM tag {old:?} in terminal, it is added automatically");
    }

    Ok(tags.iter().cloned().collect())
}

#[derive(Clone)]
struct StoreScpArgs {
    calling_ae_title: String,
    output_dir: PathBuf,
    store_port: u16,
    verbose: bool,
}

async fn run_async(store_args: StoreScpArgs) -> Result<std::convert::Infallible, snafu::Whatever> {
    std::fs::create_dir_all(&store_args.output_dir)
        .with_whatever_context(|err| format!("Could not create output directory: {err}"))?;

    let listen_addr = SocketAddrV4::new(Ipv4Addr::from(0), store_args.store_port);
    let listener = tokio::net::TcpListener::bind(listen_addr)
        .await
        .whatever_context("Could not  biond store SCP listening address")?;

    if store_args.verbose {
        info!(
            "{} listening on tcp://{listen_addr}",
            &store_args.calling_ae_title
        );
    }

    loop {
        let (socket, addr) = listener
            .accept()
            .await
            .whatever_context("Failed to set up a socket connection with source AE")?;
        let args = store_args.clone();
        tokio::spawn(async move {
            if let Err(e) = run_store_async(socket, args).await {
                warn!(
                    "Store association with {addr} failed: {}",
                    snafu::Report::from_error(e)
                );
            }
        });
    }
}
