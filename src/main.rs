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
use crate::serialize::write_responses_to_file;
use crate::store_async::run_store_async;

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
        /// Output file containing list of response study tags
        #[arg(short = 'o', long)]
        response_filepath: Option<PathBuf>,
    },
    Move {
        /// C-MOVE destination AE title
        #[arg(long = "move-destination", required = true)]
        move_destination: String,
        /// Store port to listen on
        #[arg(short = 'p', long)]
        store_port: u16,
        /// Output directory for incoming objects
        #[arg(short = 'o', long, default_value = "./output")]
        output_dir: PathBuf,
    },
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

    // Could not read DICOM command
    ReadCommand {
        source: dicom_object::ReadError,
    },

    // Could not dump DICOM output
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

    #[snafu(display("Could not write responses to file `{}`, {source}", path.display()))]
    WriteResponses {
        path: PathBuf,
        source: csv::Error,
    },

    NoPresentationContext,
    UnsupportedTransferSyntax,
    UnexpctedSCPResponse,
    #[snafu(display("No response returned"))]
    NoResponseToWrite,
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
        // out_study_file,
        information_level,
        // max_pdu_length,
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
    let res = match request_mode {
        RequestMode::Find { response_filepath } => {
            let responses = client.find_study(ds_queries)?;
            if !responses.is_empty() {
                write_responses_to_file(response_filepath, responses, tag_queries)
            } else {
                info!("No responses returned");
                Err(Error::NoResponseToWrite)
            }
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
                calling_ae_title, // was calling_ae_title.clone()
                output_dir,       // was output_dir.clone()
                store_port,
                verbose,
            };

            let handle = runtime.spawn(async move {
                let _ = run_async(store_args).await.unwrap_or_else(|err| {
                    error!("{:?}", Report::from_error(err));
                    std::process::exit(-2);
                });
            });

            let _ = client.move_study(ds_queries, &move_destination);
            handle.abort();
            Ok(())
        }
    };

    if res.is_err() {
        error!("{res:?}");
    }

    client.release_assoc();

    Ok(())
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
