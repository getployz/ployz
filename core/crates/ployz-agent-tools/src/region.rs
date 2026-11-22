//! Tools on regions: list them with their Servers, and rename one.

use serde_json::{Value, json};

use super::ToolId;
use super::act::servers;
use super::cloud::ServersAnswer;
use super::input::tool_input;
use super::machine::{Cx, Effect, Reach, Stop, Tool};
use ployz_store::{NameRegion, RegionName, RegionsQuery, region_servers};

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
        #[serde(deserialize_with = "ployz_store::deserialize_key")]
        #[schemars(with = "String", pattern(ployz_core::Region::PATTERN), example = &"default")]
        region: ployz_core::Region,
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
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({ "region": "default", "name": "Falkenstein" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let mut answers = cx.read(vec![servers()])?.into_iter();
        let answer = cx.cloud::<ServersAnswer>(answers.next())?;
        let labels = answer
            .servers
            .iter()
            .filter_map(|server| server.region.clone())
            .collect();
        let named = cx.write(&NameRegion {
            key: self.region,
            name: self.name,
            labels,
            creates: false,
        })?;
        Ok(json!({ "region": named.key, "name": named.name }))
    }
}
