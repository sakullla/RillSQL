/// 首次出示指纹时还不能发送凭据；已保存指纹变化时停止。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyDecision {
    Trusted,
    Unconfirmed { algorithm: String, fingerprint: String },
    Changed { presented: String, saved: String },
}

pub fn decide_host_key(saved: Option<&str>, presented_algorithm: &str, presented_fingerprint: &str) -> HostKeyDecision {
    match saved {
        Some(saved) if saved == presented_fingerprint => HostKeyDecision::Trusted,
        Some(saved) => HostKeyDecision::Changed {
            presented: presented_fingerprint.to_owned(),
            saved: saved.to_owned(),
        },
        None => HostKeyDecision::Unconfirmed {
            algorithm: presented_algorithm.to_owned(),
            fingerprint: presented_fingerprint.to_owned(),
        },
    }
}

pub fn credentials_allowed(decision: &HostKeyDecision) -> bool {
    matches!(decision, HostKeyDecision::Trusted)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedKey {
    pub algorithm: String,
    pub fingerprint: String,
}

#[cfg(test)]
pub trait Authenticator {
    fn authenticate(&mut self) -> Result<(), String>;
}

#[cfg(test)]
pub fn authenticate_after(decision: &HostKeyDecision, auth: &mut dyn Authenticator) -> Result<(), String> {
    match decision {
        HostKeyDecision::Trusted => auth.authenticate(),
        HostKeyDecision::Changed { presented, saved } => Err(format!(
            "服务器出示的密钥 {presented} 与已保存的 {saved} 不同。已阻止连接，没有发送任何凭据。"
        )),
        HostKeyDecision::Unconfirmed { .. } => Err("等待确认主机身份，没有发送任何凭据。".to_owned()),
    }
}
