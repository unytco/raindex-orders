use crate::config::Network;
use crate::signer::ClaimOrder;
use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use alloy::transports::{RpcError, TransportError, TransportErrorKind};
use anyhow::{anyhow, bail, Result};
use std::error::Error;

sol! {
    #[sol(rpc)]
    interface IHoloLockVault {
        function token() external view returns (address);
        function orderbook() external view returns (address);
        function vaultId() external view returns (uint256);
    }

    #[sol(rpc)]
    interface IOrderBookV3 {
        function orderExists(bytes32 orderHash) external view returns (bool);
    }
}

/// Refuses to run the bridge unless the RPC answers for `network`, and the vault
/// and claim order the configuration names are the ones deployed there. Every
/// mismatch names its variable.
pub async fn check(
    network: Network,
    rpc_url: &str,
    vault: Address,
    order: &ClaimOrder,
) -> Result<()> {
    let rpc_var = network.rpc_url_var();
    let vault_var = network.lock_vault_var();
    let url = rpc_url
        .parse()
        .map_err(|_| anyhow!("{rpc_var} is not a URL"))?;
    let provider = ProviderBuilder::new().on_http(url);
    let read_failed = |what: &str, err: TransportError| rpc_failure(rpc_var, what, &err);

    let chain_id = provider
        .get_chain_id()
        .await
        .map_err(|e| read_failed("eth_chainId", e))?;
    if chain_id != network.chain_id() {
        bail!(
            "{rpc_var} answers for chain {chain_id}, and NETWORK={} is chain {}",
            network.name(),
            network.chain_id()
        );
    }

    let code = provider
        .get_code_at(vault)
        .await
        .map_err(|e| read_failed("eth_getCode", e))?;
    if code.is_empty() {
        bail!("{vault_var} {vault} has no contract on chain {chain_id}");
    }

    let lock_vault = IHoloLockVault::new(vault, &provider);
    let call_failed = |what: &str, err: alloy::contract::Error| match err {
        alloy::contract::Error::TransportError(e) => rpc_failure(rpc_var, what, &e),
        other => anyhow!("{vault_var} {vault}: {what} failed: {other}"),
    };
    let token = lock_vault
        .token()
        .call()
        .await
        .map_err(|e| call_failed("token()", e))?
        ._0;
    let orderbook = lock_vault
        .orderbook()
        .call()
        .await
        .map_err(|e| call_failed("orderbook()", e))?
        ._0;
    let vault_id = lock_vault
        .vaultId()
        .call()
        .await
        .map_err(|e| call_failed("vaultId()", e))?
        ._0;

    let mut faults = Vec::new();
    if token != order.token {
        faults.push(format!(
            "TOKEN_ADDRESS {} is not the vault's token {token}",
            order.token
        ));
    }
    if vault_id != order.vault_id {
        faults.push(format!(
            "VAULT_ID {:#x} is not the vault's vaultId {vault_id:#x}",
            order.vault_id
        ));
    }
    if order.order_owner != vault {
        faults.push(format!(
            "ORDER_OWNER {} is not the vault {vault_var} {vault}",
            order.order_owner
        ));
    }
    if orderbook != order.orderbook {
        faults.push(format!(
            "ORDERBOOK_ADDRESS {} is not the vault's orderbook {orderbook}",
            order.orderbook
        ));
    } else {
        let exists = IOrderBookV3::new(orderbook, &provider)
            .orderExists(order.order_hash)
            .call()
            .await
            .map_err(|e| call_failed("orderExists(ORDER_HASH)", e))?
            ._0;
        if !exists {
            faults.push(format!(
                "ORDER_HASH {} is not an order on ORDERBOOK_ADDRESS {orderbook}",
                order.order_hash
            ));
        }
    }

    if !faults.is_empty() {
        bail!("{}", faults.join("; "));
    }
    Ok(())
}

/// The cause of a failed read, without the RPC URL: a provider's URL can hold its
/// API key, and reqwest names the URL in its own message.
fn rpc_failure(rpc_var: &str, what: &str, err: &TransportError) -> anyhow::Error {
    let cause = match err {
        RpcError::Transport(TransportErrorKind::Custom(inner)) => {
            match inner.downcast_ref::<reqwest::Error>() {
                Some(request) => causes(request),
                None => inner.to_string(),
            }
        }
        other => other.to_string(),
    };
    anyhow!("{rpc_var}: {what} failed: {cause}")
}

fn causes(err: &reqwest::Error) -> String {
    let mut chain = Vec::new();
    let mut source = err.source();
    while let Some(cause) = source {
        chain.push(cause.to_string());
        source = cause.source();
    }
    if chain.is_empty() {
        "the request failed".to_string()
    } else {
        chain.join(": ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256, B256, U256};
    use alloy::sol_types::{SolCall, SolValue};
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const VAULT: Address = address!("E3E064e3C2EEf66cb93dA8D8114F5084E92F48D6");
    const HOT: Address = address!("6c6EE5e31d828De241282B9606C8e98Ea48526E2");
    const ORDERBOOK: Address = address!("f1224A483ad7F1E9aA46A8CE41229F32d7549A74");
    const ORDER_HASH: B256 =
        b256!("5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9");
    const OTHER: Address = address!("1111111111111111111111111111111111111111");

    fn vault_id() -> U256 {
        "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b"
            .parse()
            .unwrap()
    }

    fn order() -> ClaimOrder {
        ClaimOrder {
            order_hash: ORDER_HASH,
            order_owner: VAULT,
            orderbook: ORDERBOOK,
            token: HOT,
            vault_id: vault_id(),
        }
    }

    /// What a JSON-RPC endpoint answers about one chain: the vault deployed with
    /// `token`, `orderbook` and `vault_id`, and the orders that orderbook holds.
    #[derive(Clone)]
    struct Chain {
        chain_id: u64,
        vault_deployed: bool,
        token: Address,
        orderbook: Address,
        vault_id: U256,
        orders: Vec<B256>,
    }

    impl Chain {
        fn of(network: Network) -> Self {
            Self {
                chain_id: network.chain_id(),
                vault_deployed: true,
                token: HOT,
                orderbook: ORDERBOOK,
                vault_id: vault_id(),
                orders: vec![ORDER_HASH],
            }
        }

        fn answer(&self, method: &str, params: &Value) -> String {
            let empty = "0x".to_string();
            match method {
                "eth_chainId" => format!("{:#x}", self.chain_id),
                "eth_getCode" if self.vault_deployed && address_at(&params[0]) == VAULT => {
                    "0x6080".to_string()
                }
                "eth_getCode" => empty,
                "eth_call" => {
                    let call = &params[0];
                    let to = address_at(&call["to"]);
                    let input = call["input"].as_str().or(call["data"].as_str()).unwrap();
                    let input = hex::decode(input.trim_start_matches("0x")).unwrap();
                    let selector: [u8; 4] = input[..4].try_into().unwrap();
                    let encoded = if to == VAULT && self.vault_deployed {
                        match selector {
                            IHoloLockVault::tokenCall::SELECTOR => self.token.abi_encode(),
                            IHoloLockVault::orderbookCall::SELECTOR => self.orderbook.abi_encode(),
                            IHoloLockVault::vaultIdCall::SELECTOR => self.vault_id.abi_encode(),
                            _ => return empty,
                        }
                    } else if to == self.orderbook
                        && selector == IOrderBookV3::orderExistsCall::SELECTOR
                    {
                        let asked = IOrderBookV3::orderExistsCall::abi_decode(&input, true)
                            .unwrap()
                            .orderHash;
                        self.orders.contains(&asked).abi_encode()
                    } else {
                        return empty;
                    };
                    format!("0x{}", hex::encode(encoded))
                }
                other => panic!("the check sent {other}"),
            }
        }
    }

    fn address_at(value: &Value) -> Address {
        value.as_str().unwrap().parse().unwrap()
    }

    /// Serves `chain` over HTTP JSON-RPC, one request per connection.
    async fn serve(chain: Chain) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let chain = chain.clone();
                tokio::spawn(async move { respond(socket, &chain).await });
            }
        });
        url
    }

    async fn respond(mut socket: TcpStream, chain: &Chain) {
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        let body = loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0, "connection closed mid-request");
            request.extend_from_slice(&chunk[..read]);
            let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map(|value| value.trim().parse().unwrap())
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                break request[end + 4..end + 4 + length].to_vec();
            }
        };
        let call: Value = serde_json::from_slice(&body).unwrap();
        let reply = json!({
            "jsonrpc": "2.0",
            "id": call["id"],
            "result": chain.answer(call["method"].as_str().unwrap(), &call["params"]),
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    }

    async fn check_against(network: Network, chain: Chain, order: &ClaimOrder) -> Result<()> {
        let url = serve(chain).await;
        check(network, &url, VAULT, order).await
    }

    async fn refusal(network: Network, chain: Chain, order: &ClaimOrder) -> String {
        format!(
            "{:#}",
            check_against(network, chain, order)
                .await
                .expect_err("the check passed")
        )
    }

    #[tokio::test]
    async fn passes_when_the_chain_holds_the_configured_vault_and_order() {
        for network in [Network::Sepolia, Network::Mainnet] {
            check_against(network, Chain::of(network), &order())
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn refuses_an_rpc_that_answers_for_the_other_network() {
        assert_eq!(
            refusal(Network::Mainnet, Chain::of(Network::Sepolia), &order()).await,
            "ETH_RPC_URL answers for chain 11155111, and NETWORK=mainnet is chain 1"
        );
        assert_eq!(
            refusal(Network::Sepolia, Chain::of(Network::Mainnet), &order()).await,
            "SEPOLIA_RPC_URL answers for chain 1, and NETWORK=sepolia is chain 11155111"
        );
    }

    #[tokio::test]
    async fn refuses_a_vault_with_no_contract() {
        let chain = Chain {
            vault_deployed: false,
            ..Chain::of(Network::Mainnet)
        };

        assert_eq!(
            refusal(Network::Mainnet, chain, &order()).await,
            format!("MAINNET_LOCK_VAULT_ADDRESS {VAULT} has no contract on chain 1")
        );
    }

    #[tokio::test]
    async fn names_each_value_the_vault_does_not_hold() {
        let differing = [
            (
                Chain {
                    token: OTHER,
                    ..Chain::of(Network::Mainnet)
                },
                format!("TOKEN_ADDRESS {HOT} is not the vault's token {OTHER}"),
            ),
            (
                Chain {
                    vault_id: U256::from(1),
                    ..Chain::of(Network::Mainnet)
                },
                format!("VAULT_ID {:#x} is not the vault's vaultId 0x1", vault_id()),
            ),
            (
                Chain {
                    orderbook: OTHER,
                    ..Chain::of(Network::Mainnet)
                },
                format!("ORDERBOOK_ADDRESS {ORDERBOOK} is not the vault's orderbook {OTHER}"),
            ),
        ];
        for (chain, expected) in differing {
            assert_eq!(refusal(Network::Mainnet, chain, &order()).await, expected);
        }
    }

    #[tokio::test]
    async fn refuses_an_order_owner_that_is_not_the_vault() {
        let order = ClaimOrder {
            order_owner: OTHER,
            ..order()
        };

        assert_eq!(
            refusal(Network::Sepolia, Chain::of(Network::Sepolia), &order).await,
            format!("ORDER_OWNER {OTHER} is not the vault SEPOLIA_LOCK_VAULT_ADDRESS {VAULT}")
        );
    }

    #[tokio::test]
    async fn refuses_an_order_hash_the_orderbook_does_not_hold() {
        let chain = Chain {
            orders: vec![],
            ..Chain::of(Network::Mainnet)
        };

        assert_eq!(
            refusal(Network::Mainnet, chain, &order()).await,
            format!("ORDER_HASH {ORDER_HASH} is not an order on ORDERBOOK_ADDRESS {ORDERBOOK}")
        );
    }

    #[tokio::test]
    async fn names_every_mismatch_in_one_refusal() {
        let chain = Chain {
            token: OTHER,
            orders: vec![],
            ..Chain::of(Network::Mainnet)
        };
        let order = ClaimOrder {
            order_owner: OTHER,
            ..order()
        };

        let message = refusal(Network::Mainnet, chain, &order).await;

        for variable in ["TOKEN_ADDRESS", "ORDER_OWNER", "ORDER_HASH"] {
            assert!(message.contains(variable), "{message}");
        }
    }

    #[tokio::test]
    async fn refuses_an_rpc_it_cannot_reach_without_naming_its_url() {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v3/secret-api-key", closed.local_addr().unwrap());
        drop(closed);

        let err = check(Network::Mainnet, &url, VAULT, &order())
            .await
            .expect_err("an unreachable RPC passed the check");
        let message = format!("{err:#}");

        assert!(
            message.starts_with("ETH_RPC_URL: eth_chainId failed: "),
            "{message}"
        );
        assert!(!message.contains("secret-api-key"), "{message}");
    }
}
