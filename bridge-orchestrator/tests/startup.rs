use holo_hash::{AgentPubKey, AgentPubKeyB64};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

/// A mainnet node whose Ethereum RPC refuses every connection.
fn node(dir: &Path) -> Vec<(&'static str, String)> {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let rpc = format!("http://{}/", closed.local_addr().unwrap());
    drop(closed);
    let agent: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![1; 32]).into();
    vec![
        ("NETWORK", "mainnet".to_string()),
        ("ETH_RPC_URL", rpc),
        (
            "MAINNET_LOCK_VAULT_ADDRESS",
            "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6".to_string(),
        ),
        ("HOLOCHAIN_BRIDGING_AGENT_PUBKEY", agent.to_string()),
        ("DB_PATH", db(dir).display().to_string()),
    ]
}

fn signer() -> Vec<(&'static str, String)> {
    [
        (
            "ORDER_HASH",
            "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9",
        ),
        ("ORDER_OWNER", "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6"),
        (
            "ORDERBOOK_ADDRESS",
            "0xf1224A483ad7F1E9aA46A8CE41229F32d7549A74",
        ),
        (
            "TOKEN_ADDRESS",
            "0x6c6EE5e31d828De241282B9606C8e98Ea48526E2",
        ),
        (
            "VAULT_ID",
            "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b",
        ),
        (
            "SIGNER_PRIVATE_KEY",
            "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
        ),
        ("CLAIM_SIGNER", "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"),
        (
            "CLAIM_INTERPRETER",
            "0x4C7436641da0505A8012218c1524Db0060Fd7253",
        ),
        ("CLAIM_STORE", "0x32a868432101C516647E7Ee217CA641B288953C6"),
        (
            "CLAIM_EXPRESSION",
            "0x1e814F560938B7Ed82Ba00Cc075a822E4789309E",
        ),
        (
            "CLAIM_INPUT_TOKEN",
            "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key, value.to_string()))
    .collect()
}

fn db(dir: &Path) -> std::path::PathBuf {
    dir.join("bridge_orchestrator.db")
}

fn orchestrator(dir: &Path, env: &[(&str, String)], args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bridge-orchestrator"))
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .output()
        .unwrap()
}

#[test]
fn status_and_clear_read_no_chain_and_need_no_signer() {
    let dir = tempfile::tempdir().unwrap();
    let env = node(dir.path());

    for args in [&["status"][..], &["clear", "--all"][..]] {
        let output = orchestrator(dir.path(), &env, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn run_names_the_unset_signer_variables_before_it_writes_anything() {
    let dir = tempfile::tempdir().unwrap();

    let output = orchestrator(dir.path(), &node(dir.path()), &["run"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    for variable in ["ORDER_HASH", "SIGNER_PRIVATE_KEY"] {
        assert!(
            stderr.contains(&format!("{variable} is required")),
            "{stderr}"
        );
    }
    assert!(!db(dir.path()).exists());
}

#[test]
fn run_stops_on_an_unreachable_rpc_before_it_writes_anything() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = node(dir.path());
    env.extend(signer());

    let output = orchestrator(dir.path(), &env, &["run"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("ETH_RPC_URL: eth_chainId failed"),
        "{stderr}"
    );
    assert!(!db(dir.path()).exists());
}

#[test]
fn run_on_mainnet_refuses_the_test_signer_before_it_writes_anything() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = node(dir.path());
    env.extend(signer());
    env.retain(|(key, _)| *key != "SIGNER_PRIVATE_KEY");
    env.push((
        "SIGNER_PRIVATE_KEY",
        "0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd".to_string(),
    ));

    let output = orchestrator(dir.path(), &env, &["run"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("SIGNER_PRIVATE_KEY is the test signer"),
        "{stderr}"
    );
    assert!(!db(dir.path()).exists());
}
