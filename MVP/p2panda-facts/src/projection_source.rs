use super::*;

impl FactSource for PandaFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        if island != session.island() {
            return Ok(Vec::new());
        }
        Ok(self
            .derived_index
            .list_candidates(self.authorizer.as_ref(), island, pattern, session))
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        if island != session.island() {
            return Ok(BTreeMap::new());
        }
        Ok(self
            .derived_index
            .read_payloads(self.authorizer.as_ref(), island, candidates, session))
    }
}

impl FactSource for SharedPandaFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.store
            .try_lock()
            .map_err(|_| self.unavailable())?
            .list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.store
            .try_lock()
            .map_err(|_| self.unavailable())?
            .read_payloads(island, candidates, session)
    }
}
