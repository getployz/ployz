#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    WireGuard,
    Corrosion,
    Pingora,
}

pub fn startup_order(enable_corrosion: bool, enable_pingora: bool) -> Vec<ServiceKind> {
    let mut order = vec![ServiceKind::WireGuard];
    if enable_corrosion {
        order.push(ServiceKind::Corrosion);
    }
    if enable_pingora {
        if !enable_corrosion {
            order.push(ServiceKind::Corrosion);
        }
        order.push(ServiceKind::Pingora);
    }
    order
}

pub fn shutdown_order(enable_corrosion: bool, enable_pingora: bool) -> Vec<ServiceKind> {
    let mut order = startup_order(enable_corrosion, enable_pingora);
    order.reverse();
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pingora_requires_corrosion_before_it() {
        let order = startup_order(false, true);
        assert_eq!(
            order,
            vec![
                ServiceKind::WireGuard,
                ServiceKind::Corrosion,
                ServiceKind::Pingora
            ]
        );
    }

    #[test]
    fn shutdown_reverses_startup() {
        let startup = startup_order(true, true);
        let shutdown = shutdown_order(true, true);
        assert_eq!(
            shutdown,
            vec![
                ServiceKind::Pingora,
                ServiceKind::Corrosion,
                ServiceKind::WireGuard
            ]
        );
        assert_eq!(startup.into_iter().rev().collect::<Vec<_>>(), shutdown);
    }
}
