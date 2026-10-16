//! The loop the unit starts must run the ployzd `--install` chose.

use std::path::{Path, PathBuf};

use clap::Command;

use super::{command, loop_ployzd};
use crate::checkin::{Paths, unit};

fn exec_start(text: &str) -> Vec<String> {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .unwrap();
    line.split('"')
        .enumerate()
        .flat_map(|(at, part)| {
            if at % 2 == 1 {
                vec![part.to_owned()]
            } else {
                part.split_whitespace().map(str::to_owned).collect()
            }
        })
        .collect()
}

#[tokio::test]
async fn the_unit_starts_the_loop_with_the_ployzd_install_chose() {
    let chosen = Path::new("/var/lib/ployz/checkin/ployzd");
    let text = unit::checkin(
        Path::new("/usr/local/bin/ployz"),
        chosen,
        Path::new("/etc/ployz/fleet-token"),
        "https://cloud.example",
    )
    .unwrap();
    let args = exec_start(&text);
    assert_eq!(args.get(1..3).unwrap(), ["server", "checkin"]);
    let matches = command(Command::new("checkin"))
        .no_binary_name(true)
        .try_get_matches_from(args.get(3..).unwrap())
        .unwrap();
    let paths = Paths::system(PathBuf::from("/etc/ployz/fleet-token"));
    assert_eq!(loop_ployzd(&matches, &paths).await.unwrap(), chosen);
}
