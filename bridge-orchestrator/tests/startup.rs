use holo_hash::{AgentPubKey, AgentPubKeyB64};
use std::io::{Read, Write};
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
        durable_conductor(dir),
    ]
}

fn durable_conductor(dir: &Path) -> (&'static str, String) {
    let config = dir.join("conductor-config.yaml");
    std::fs::write(
        &config,
        "keystore:\n  type: lair_server\n  connection_url: unix:///x/socket?k=abc\ndb_sync_level: Full\n",
    )
    .unwrap();
    ("CONDUCTOR_CONFIG", config.display().to_string())
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

const IN_TRANSIT: [&[&str]; 2] = [&["in-transit"], &["in-transit", "--mark-failed"]];

#[test]
fn in_transit_refuses_a_database_that_is_not_there() {
    let dir = tempfile::tempdir().unwrap();

    for args in IN_TRANSIT {
        let output = orchestrator(dir.path(), &node(dir.path()), args);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains("does not exist"), "{args:?}: {stderr}");
        assert!(!db(dir.path()).exists());
    }
}

#[test]
fn in_transit_fails_when_the_conductor_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let passphrase = dir.path().join("lair-passphrase");
    std::fs::write(&passphrase, "deadbeef\n").unwrap();
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port().to_string();
    drop(closed);
    let mut env = node(dir.path());
    env.extend([
        ("LAIR_PASSPHRASE_FILE", passphrase.display().to_string()),
        ("HOLOCHAIN_ADMIN_PORT", port.clone()),
        ("HOLOCHAIN_APP_PORT", port),
    ]);
    assert!(orchestrator(dir.path(), &env, &["status"]).status.success());

    for args in IN_TRANSIT {
        let output = orchestrator(dir.path(), &env, args);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr.contains("Failed to connect to Holochain"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn status_and_clear_refuse_an_unknown_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = node(dir.path());
    env.retain(|(key, _)| *key != "NETWORK");
    env.push(("NETWORK", "goerli".to_string()));

    for args in [&["status"][..], &["clear", "--all"][..]] {
        let output = orchestrator(dir.path(), &env, args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr.contains("NETWORK=goerli is not sepolia, mainnet or none"),
            "{args:?}: {stderr}"
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

/// An RPC that answers every call with chain 1, as an Ethereum mainnet RPC does.
fn mainnet_rpc() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut socket in listener.incoming().flatten() {
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            while let Ok(read) = socket.read(&mut chunk) {
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request);
                if read == 0 || text.ends_with('}') {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request);
            let id = text
                .split("\"id\":")
                .nth(1)
                .and_then(|rest| rest.split([',', '}']).next())
                .unwrap_or("0")
                .to_string();
            let body = format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":\"0x1\"}}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
        }
    });
    url
}

/// A sepolia node told only its agent, its key and where its RPC is.
fn sepolia_node(dir: &Path, rpc: String) -> Vec<(&'static str, String)> {
    let agent: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![1; 32]).into();
    vec![
        ("SEPOLIA_RPC_URL", rpc),
        ("HOLOCHAIN_BRIDGING_AGENT_PUBKEY", agent.to_string()),
        ("DB_PATH", db(dir).display().to_string()),
        durable_conductor(dir),
        (
            "SIGNER_PRIVATE_KEY",
            "0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd".to_string(),
        ),
    ]
}

#[test]
fn run_with_nothing_set_but_its_key_takes_testnet_values_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let rpc = format!("http://{}/", closed.local_addr().unwrap());
    drop(closed);

    let output = orchestrator(dir.path(), &sepolia_node(dir.path(), rpc), &["run"]);

    let logged = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success());
    assert!(logged.contains("NETWORK is unset: sepolia"), "{logged}");
    assert!(
        logged.contains("using TestNet's values for SEPOLIA_LOCK_VAULT_ADDRESS"),
        "{logged}"
    );
    assert!(
        logged.contains("using TestNet's values for ORDER_HASH, ORDER_OWNER, ORDERBOOK_ADDRESS, TOKEN_ADDRESS, VAULT_ID, CLAIM_SIGNER, CLAIM_INTERPRETER, CLAIM_STORE, CLAIM_EXPRESSION, CLAIM_INPUT_TOKEN"),
        "{logged}"
    );
    assert!(
        logged.contains("SEPOLIA_RPC_URL: eth_chainId failed"),
        "{logged}"
    );
    assert!(!db(dir.path()).exists());
}

#[test]
fn a_run_on_testnet_values_refuses_a_mainnet_rpc() {
    let dir = tempfile::tempdir().unwrap();

    let output = orchestrator(
        dir.path(),
        &sepolia_node(dir.path(), mainnet_rpc()),
        &["run"],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr
            .contains("SEPOLIA_RPC_URL answers for chain 1, and NETWORK=sepolia is chain 11155111"),
        "{stderr}"
    );
    assert!(!db(dir.path()).exists());
}

#[test]
fn run_with_ethereum_off_checks_no_chain_takes_no_testnet_value_and_names_what_it_ignores() {
    let dir = tempfile::tempdir().unwrap();
    let rpc = TcpListener::bind("127.0.0.1:0").unwrap();
    rpc.set_nonblocking(true).unwrap();
    let mut env = sepolia_node(dir.path(), format!("http://{}/", rpc.local_addr().unwrap()));
    env.extend([
        ("NETWORK", "none".to_string()),
        (
            "SEPOLIA_LOCK_VAULT_ADDRESS",
            format!("{:#x}", alloy::primitives::Address::ZERO),
        ),
        (
            "ORDER_HASH",
            format!("{:#x}", alloy::primitives::B256::ZERO),
        ),
        (
            "LAIR_PASSPHRASE_FILE",
            dir.path().join("no-passphrase").display().to_string(),
        ),
    ]);

    let output = orchestrator(dir.path(), &env, &["run"]);

    let logged = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(logged.contains("NETWORK=none: Ethereum is off"), "{logged}");
    assert!(
        logged.contains("NETWORK=none ignores SEPOLIA_RPC_URL, SEPOLIA_LOCK_VAULT_ADDRESS, ORDER_HASH, SIGNER_PRIVATE_KEY"),
        "{logged}"
    );
    assert!(
        logged.contains("bridge-orchestrator started network=none"),
        "{logged}"
    );
    assert!(!logged.contains("TestNet"), "{logged}");
    assert!(logged.contains("no-passphrase"), "{logged}");
    assert!(
        matches!(rpc.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "NETWORK=none connected to the RPC"
    );
}

#[test]
fn run_refuses_a_conductor_a_power_loss_can_roll_back() {
    let dir = tempfile::tempdir().unwrap();
    let conductor_config = dir.path().join("conductor-config.yaml");
    std::fs::write(
        &conductor_config,
        "keystore:\n  type: lair_server\n  connection_url: unix:///x/socket?k=abc\n",
    )
    .unwrap();
    let passphrase = dir.path().join("lair-passphrase");
    std::fs::write(&passphrase, "deadbeef\n").unwrap();
    let agent: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![1; 32]).into();
    let env = [
        ("NETWORK", "none".to_string()),
        ("HOLOCHAIN_BRIDGING_AGENT_PUBKEY", agent.to_string()),
        ("DB_PATH", db(dir.path()).display().to_string()),
        ("CONDUCTOR_CONFIG", conductor_config.display().to_string()),
        ("LAIR_PASSPHRASE_FILE", passphrase.display().to_string()),
    ];

    let mut run = Command::new(env!("CARGO_BIN_EXE_bridge-orchestrator"))
        .arg("run")
        .current_dir(dir.path())
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while run.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            run.kill().unwrap();
            run.wait().unwrap();
            panic!("run went on with a conductor a power loss can roll back");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let output = run.wait_with_output().unwrap();

    let logged = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "{logged}");
    assert!(
        logged.contains("must set `db_sync_level: Full`"),
        "{logged}"
    );
    assert!(
        !db(dir.path()).exists(),
        "the refusal came before the database was opened"
    );
}
