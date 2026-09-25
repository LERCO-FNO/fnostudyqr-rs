use clap::{Parser, Subcommand, ValueEnum};
use snafu::{Report, Whatever, prelude::*};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use tracing::{error, info, warn};

mod client;
mod query;
mod serialize;
mod store_async;
mod utils;

use crate::client::ScuClient;
use crate::query::*;
use crate::serialize::serialize_responses;
use crate::store_async::run_store_async;
use crate::utils::{construct_filepath, validate_response_filepath};

/// DICOM C-FIND/C-MOVE application
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Socket address to SCP, ex: "<AET>@127.0.0.1:1045".
    /// Called AET prefix is optional, otherwise used with --called_ae_title=<AET>
    addr: String,
    /// Input file containing list of study tags.
    /// Minimum of PatientID and StudyDate are required
    #[arg(short = 'i', long, global = true)]
    in_study_file: Option<PathBuf>,
    /// Additional sequence of tags
    #[arg(short = 'q', long, global = true)]
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
    /// Path to file/directory to write response tags
    #[arg(short = 'f', long, /*default_value = "./",*/ value_parser = validate_response_filepath)]
    out_response_filepath: Option<PathBuf>,
    /// Response file extension
    #[arg(short = 'e', long, default_value = "csv")]
    file_extension: FileExtension,
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
    Find,
    Move {
        /// C-MOVE destination AE title. Defaults to --calling-ae-title
        #[arg(long = "move-destination")]
        move_destination: Option<String>,
        /// Store port to listen on
        #[arg(short = 'p', long)]
        store_port: u16,
        /// Output directory for incoming objects
        #[arg(short = 'o', long, default_value = "./output")]
        output_dir: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FileExtension {
    Csv,
    Json,
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
    #[snafu(display("File not found at `{}`", file.display()))]
    FileNotFound {
        source: csv::Error,
        file: PathBuf,
    },
    #[snafu(display("Could not create datasets from file"))]
    DatasetsFromFile,

    NoPresentationContext,

    UnsupportedTransferSyntax,

    UnexpctedSCPResponse,

    NoResponsesToWrite,

    #[snafu(display("No response returned"))]
    NoResponseToWrite,

    #[snafu(display("Could not create output file `{}`, {source}", path.display()))]
    CreateOutputFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("{source}"))]
    SerializeJson {
        source: serde_json::Error,
    },
    #[snafu(display("{source}"))]
    SerializeCsv {
        source: csv::Error,
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
        out_response_filepath,
        file_extension,
        verbose,
    } = Args::parse();

    tracing::subscriber::set_global_default(
        tracing_subscriber::FmtSubscriber::builder()
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

    let query_tags = parse_query_tags(query_tag)
        .whatever_context("Failed to parse query tags from command line")?;
    let (ds_queries, tag_queries) =
        build_queries(in_study_file, query_tags, &information_level, verbose)?;

    let mut client = ScuClient::new(
        (&request_mode).into(),
        addr,
        information_level,
        calling_ae_title.clone(),
        called_ae_title,
        verbose,
    )?;

    info!("Requesting {} query", ds_queries.len());
    let query_result = match request_mode {
        RequestMode::Find => client.find_study(ds_queries),
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
                calling_ae_title: calling_ae_title.clone(), // was calling_ae_title.clone()
                output_dir,                                 // was output_dir.clone()
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
            let res = client.move_study(ds_queries, &move_destination);
            handle.abort();
            res
        }
    };

    client.release_assoc();

    let responses = match query_result {
        Ok(responses) => responses,
        Err(Error::NoResponsesToWrite) => {
            info!("No responses to write due to no matches");
            return Ok(());
        }
        Err(err) => {
            error!("{err}");
            return Ok(());
        }
    };

    let out_file_path = if let Some(out_file_path) = out_response_filepath {
        construct_filepath(out_file_path, file_extension)
    } else {
        info!("Responses received but no output path given, skipping write");
        return Ok(());
    };

    serialize_responses(out_file_path, responses, tag_queries, file_extension)
}

fn parse_query_tags(query_tags: Vec<String>) -> Result<Vec<TermQuery>, Whatever> {
    let mut tags = query_tags
        .iter()
        .map(|t| t.parse::<TermQuery>())
        .collect::<Result<Vec<TermQuery>, _>>()
        .whatever_context("Could not parse query tags")?;
    let study_tag: TermQuery = "StudyInstanceUID".parse().unwrap();

    // always add StudyInstanceUID to be part of responses
    if !tags.iter().any(|t| t.selector == study_tag.selector) {
        tags.insert(0, study_tag);
    }

    Ok(tags)
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

#[cfg(test)]
mod tests {}
