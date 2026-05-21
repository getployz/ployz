use super::*;

impl MemoryBus {
    pub fn write_fact(
        &self,
        session: &BusSession,
        key: FactKey,
        content_hash: FactContentHash,
    ) -> Result<FactWriteOutcome> {
        let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
        if !inner
            .grants
            .can_write_fact(session.island(), session.principal(), &key)
        {
            return Err(BusError::UnauthorizedFactWrite {
                island: session.island().clone(),
                principal: session.principal().clone(),
                key: Box::new(key),
            });
        }
        inner.facts.write(
            session.island().clone(),
            session.principal().clone(),
            key,
            content_hash,
        )
    }

    pub fn write_fact_payload(
        &self,
        session: &BusSession,
        key: FactKey,
        payload: impl Into<FactPayload>,
    ) -> Result<FactWriteOutcome> {
        let payload = payload.into();
        let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
        if !inner
            .grants
            .can_write_fact(session.island(), session.principal(), &key)
        {
            return Err(BusError::UnauthorizedFactWrite {
                island: session.island().clone(),
                principal: session.principal().clone(),
                key: Box::new(key),
            });
        }
        inner.facts.write_payload(
            session.island().clone(),
            session.principal().clone(),
            key,
            payload,
        )
    }

    pub fn read_fact(&self, session: &BusSession, key: &FactKey) -> Result<Option<Fact>> {
        let inner = self.inner.lock().expect("memory bus mutex poisoned");
        if !inner
            .grants
            .can_read_fact(session.island(), session.principal(), key)
        {
            return Err(BusError::UnauthorizedFactRead {
                island: session.island().clone(),
                principal: session.principal().clone(),
                key: Box::new(key.clone()),
            });
        }
        Ok(inner.facts.read(session.island(), key))
    }

    pub fn list_facts(&self, session: &BusSession, pattern: &FactKeyPattern) -> Result<Vec<Fact>> {
        let inner = self.inner.lock().expect("memory bus mutex poisoned");
        Ok(inner
            .facts
            .list_filtered(session.island(), pattern, |fact| {
                inner
                    .grants
                    .can_read_fact(session.island(), session.principal(), fact.key())
            }))
    }

    pub fn read_fact_payload(
        &self,
        session: &BusSession,
        key: &FactKey,
    ) -> Result<Option<FactPayload>> {
        let inner = self.inner.lock().expect("memory bus mutex poisoned");
        if !inner
            .grants
            .can_read_fact(session.island(), session.principal(), key)
        {
            return Err(BusError::UnauthorizedFactRead {
                island: session.island().clone(),
                principal: session.principal().clone(),
                key: Box::new(key.clone()),
            });
        }
        Ok(inner.facts.read(session.island(), key).and_then(|fact| {
            inner
                .facts
                .payload(session.island(), key, fact.content_hash())
        }))
    }

    pub fn read_fact_payloads(
        &self,
        session: &BusSession,
        requests: &[(FactKey, FactContentHash)],
    ) -> Result<BTreeMap<FactContentHash, FactPayload>> {
        let inner = self.inner.lock().expect("memory bus mutex poisoned");
        let mut payloads = BTreeMap::new();
        for (key, expected_hash) in requests {
            if !inner
                .grants
                .can_read_fact(session.island(), session.principal(), key)
            {
                return Err(BusError::UnauthorizedFactRead {
                    island: session.island().clone(),
                    principal: session.principal().clone(),
                    key: Box::new(key.clone()),
                });
            }
            let Some(fact) = inner.facts.read_exact(session.island(), key, expected_hash) else {
                continue;
            };
            if let Some(payload) = inner
                .facts
                .payload(session.island(), key, fact.content_hash())
            {
                payloads.insert(expected_hash.clone(), payload);
            }
        }
        Ok(payloads)
    }
}

impl FactAuthorizer for MemoryBus {
    fn can_access_fact(&self, session: &BusSession, key: &FactKey, access: FactAccess) -> bool {
        self.can_principal_access_fact(session.island(), session.principal(), key, access)
    }

    fn can_principal_access_fact(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        key: &FactKey,
        access: FactAccess,
    ) -> bool {
        let inner = self.inner.lock().expect("memory bus mutex poisoned");
        match access {
            FactAccess::Read => inner.grants.can_read_fact(island, principal, key),
            FactAccess::Write => inner.grants.can_write_fact(island, principal, key),
        }
    }
}
