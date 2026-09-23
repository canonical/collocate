use collocate_core::net::{Proto, Publish};
use collocate_core::ContainerId;
use collocate_net::ipam::Subnet;
use collocate_net::ruleset::{render, Algorithm, Backend, ContainerPorts, LbRule, NoBackends, Ruleset};
use std::io::Write;
use std::process::{Command, Stdio};

fn privileged() -> bool {
    std::env::var("COLLOCATE_PRIV_TESTS").as_deref() == Ok("1")
}

fn nft_check(script: &str) -> Result<(), String> {
    let mut child = Command::new("sudo")
        .args(["-n", "nft", "-c", "-f", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

fn ruleset(algorithm: Algorithm, backends: Vec<Backend>, proto: Proto, nb: NoBackends) -> Ruleset {
    Ruleset {
        bridge: "collocate0".into(),
        subnet: Subnet::parse("172.30.0.0/16").unwrap(),
        containers: vec![ContainerPorts {
            id: ContainerId::from_bytes([0xab; 6]),
            addr: "172.30.0.2".parse().unwrap(),
            publish: vec![Publish::parse("8080:80").unwrap(), Publish::parse("5353:53/udp").unwrap()],
        }],
        lbs: vec![LbRule {
            name: "app/web".into(),
            vip: "172.30.255.1".parse().unwrap(),
            proto,
            listen: 80,
            publish: vec![8081],
            algorithm,
            backends,
            on_no_backends: nb,
        }],
    }
}

fn be(a: &str, w: u32) -> Backend {
    Backend { addr: a.parse().unwrap(), port: 8080, weight: w }
}

#[test]
fn kernel_accepts_every_rendering_variant() {
    if !privileged() {
        return;
    }
    let backends = vec![be("172.30.0.4", 2), be("172.30.0.5", 1)];
    for alg in [Algorithm::RoundRobin, Algorithm::Random, Algorithm::SourceHash] {
        let script = render(&ruleset(alg, backends.clone(), Proto::Tcp, NoBackends::Reject));
        nft_check(&script).unwrap_or_else(|e| panic!("{alg:?}: {e}\n{script}"));
    }
    for (proto, nb) in [(Proto::Tcp, NoBackends::Reject), (Proto::Tcp, NoBackends::Drop), (Proto::Udp, NoBackends::Reject)] {
        let script = render(&ruleset(Algorithm::RoundRobin, vec![], proto, nb));
        nft_check(&script).unwrap_or_else(|e| panic!("{proto:?}/{nb:?}: {e}\n{script}"));
    }
    let script = render(&ruleset(Algorithm::RoundRobin, backends, Proto::Udp, NoBackends::Reject));
    nft_check(&script).unwrap_or_else(|e| panic!("udp lb: {e}\n{script}"));
}
