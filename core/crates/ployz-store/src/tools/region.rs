//! Tools on regions: list them with their Servers, and rename one.

use serde_json::{Value, json};

use super::ToolId;
use super::act::servers;
use super::cloud::ServersAnswer;
use super::input::tool_input;
use super::machine::{Cx, Effect, Reach, Stop, Tool};
use crate::{NameRegion, RegionKey, RegionName, RegionsQuery, region_servers};

/// What a region key is, for a model to send.
const REGION_KEY: &str = "^[a-z0-9][a-z0-9-]{0,62}$";

tool_input! {
    pub(crate) struct RegionList {}
}

impl Tool for RegionList {
    const NAME: &'static str = "region_list";
    const STATUS: ToolId = ToolId::RegionList;
    const TITLE: &'static str = "List regions";
    const DESCRIPTION: &'static str = "List regions: name, key and Servers, and the Servers in no region. New Servers join the default region.";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({})
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let mut answers = cx.read(vec![servers()])?.into_iter();
        let answer = cx.cloud::<ServersAnswer>(answers.next())?;
        let servers = answer
            .servers
            .iter()
            .map(|server| (server.name.as_str(), server.region.as_deref()))
            .collect::<Vec<_>>();
        let labels = answer
            .servers
            .iter()
            .filter_map(|server| server.region.clone())
            .collect();
        let view = cx.one(&RegionsQuery { labels })?;
        let listed = region_servers(view.regions, &servers);
        Ok(json!({ "regions": listed.regions, "no_region": listed.no_region, "seen": answer.seen }))
    }
}

tool_input! {
    pub(crate) struct RegionRename {
        /// The region's key, the `region` label its Servers carry, e.g. `default`.
        #[schemars(with = "String", pattern(REGION_KEY), example = &"default")]
        region: RegionKey,
        /// Its new name, one line of 1 to 40 characters.
        #[schemars(with = "String", length(min = 1, max = 40), example = &"Falkenstein")]
        name: RegionName,
    }
}

impl Tool for RegionRename {
    const NAME: &'static str = "region_rename";
    const STATUS: ToolId = ToolId::RegionList;
    const TITLE: &'static str = "Rename a region";
    const DESCRIPTION: &'static str =
        "Rename a region. Its key, Servers and Environments stay as they are, so nothing restarts.";
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "region": "default", "name": "Falkenstein" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let named = cx.write(&NameRegion {
            key: self.region,
            name: self.name,
        })?;
        Ok(json!({ "region": named.key, "name": named.name }))
    }
}
