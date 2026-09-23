use collocate_core::spec::Spec;

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

pub fn build_env(spec: &Spec) -> Vec<String> {
    let mut env: Vec<String> = spec.process.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let has = |env: &[String], key: &str| env.iter().any(|e| e.starts_with(&format!("{key}=")));
    if !has(&env, "PATH") {
        env.push(format!("PATH={DEFAULT_PATH}"));
    }
    if !has(&env, "HOSTNAME") {
        env.push(format!("HOSTNAME={}", spec.hostname));
    }
    if !has(&env, "HOME") {
        env.push("HOME=/root".to_string());
    }
    env
}

pub fn merge_env(mut base: Vec<String>, overrides: &[(String, String)]) -> Vec<String> {
    for (k, v) in overrides {
        let prefix = format!("{k}=");
        match base.iter_mut().find(|e| e.starts_with(&prefix)) {
            Some(slot) => *slot = format!("{k}={v}"),
            None => base.push(format!("{k}={v}")),
        }
    }
    base
}
