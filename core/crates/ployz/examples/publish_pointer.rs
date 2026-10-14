//! Publish one Serving Pointer by hand, for live checks.
//!
//! ```text
//! publish_pointer <namespace/service>                      print the current pointer
//! publish_pointer <namespace/service> <pointer> <read>     compare-and-set
//! ```
//!
//! `<pointer>` is pointer JSON. `<read>` is the pointer JSON the write assumes, `none` for a
//! first publish, or `current` to read it first. Prints `Published` or
//! `Superseded <current>`. Connects like the CLI: `PLOYZ_CONNECT`, else `PLOYZ_CONTEXT`,
//! else the local daemon, with `PLOYZ_CONFIG` (default `~/.config/ployz/config.yaml`).

use std::path::PathBuf;
use std::process::ExitCode;

use ployz::connect::connect;
use ployz_core::{
    PublishServingPointerRequest, QualifiedService, ServingPointer, ServingPointerPublication,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let service = QualifiedService::parse(
        args.first()
            .ok_or("usage: publish_pointer <namespace/service> [<pointer> <read>]")?,
    )?;
    let config = std::env::var("PLOYZ_CONFIG").map_or_else(
        |_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".config/ployz/config.yaml")
        },
        PathBuf::from,
    );
    let mut client = connect(
        &config,
        std::env::var("PLOYZ_CONNECT").ok().as_deref(),
        std::env::var("PLOYZ_CONTEXT").ok().as_deref(),
    )
    .await?;
    let [_, pointer, read] = args.as_slice() else {
        println!(
            "{}",
            serde_json::to_string(&client.serving_pointer(&service).await?)?
        );
        return Ok(());
    };
    let pointer = serde_json::from_str::<ServingPointer>(pointer)?;
    let read = match read.as_str() {
        "none" => None,
        "current" => client.serving_pointer(&service).await?,
        read => Some(serde_json::from_str::<ServingPointer>(read)?),
    };
    match client
        .publish_serving_pointer(PublishServingPointerRequest {
            service,
            read,
            pointer,
        })
        .await?
    {
        ServingPointerPublication::Published => println!("Published"),
        ServingPointerPublication::Superseded { current } => {
            println!("Superseded {}", serde_json::to_string(&current)?);
        }
    }
    Ok(())
}
