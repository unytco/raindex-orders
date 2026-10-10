use crate::config::Network;
use crate::signer::{ClaimOrder, CouponSigner, SignedCoupon};
use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use alloy::sol_types::{Revert, SolError, SolValue};
use alloy::transports::{RpcError, TransportError, TransportErrorKind};
use anyhow::{anyhow, bail, Result};
use std::error::Error;
use std::time::Duration;

sol! {
    struct IO {
        address token;
        uint8 decimals;
        uint256 vaultId;
    }

    struct EvaluableV2 {
        address interpreter;
        address store;
        address expression;
    }

    struct OrderV2 {
        address owner;
        bool handleIO;
        EvaluableV2 evaluable;
        IO[] validInputs;
        IO[] validOutputs;
    }

    struct SignedContextV1 {
        address signer;
        uint256[] context;
        bytes signature;
    }

    struct TakeOrderConfigV2 {
        OrderV2 order;
        uint256 inputIOIndex;
        uint256 outputIOIndex;
        SignedContextV1[] signedContext;
    }

    struct TakeOrdersConfigV2 {
        uint256 minimumInput;
        uint256 maximumInput;
        uint256 maximumIORatio;
        TakeOrderConfigV2[] orders;
        bytes data;
    }

    error MinimumInput(uint256 minimumInput, uint256 input);

    #[sol(rpc)]
    interface IHoloLockVault {
        function token() external view returns (address);
        function orderbook() external view returns (address);
        function vaultId() external view returns (uint256);
    }

    #[sol(rpc)]
    interface IOrderBookV3 {
        function orderExists(bytes32 orderHash) external view returns (bool);
        function takeOrders(TakeOrdersConfigV2 calldata config)
            external
            returns (uint256 totalTakerInput, uint256 totalTakerOutput);
    }
}

pub const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Refuses to run the bridge unless the RPC answers for `network`, the vault and
/// claim order the configuration names are the ones deployed there, and that order
/// accepts coupons signed with the loaded key. Every mismatch names its variable.
pub async fn check(
    network: Network,
    rpc_url: &str,
    vault: Address,
    signer: &CouponSigner,
) -> Result<()> {
    check_within(RPC_TIMEOUT, network, rpc_url, vault, signer).await
}

async fn check_within(
    timeout: Duration,
    network: Network,
    rpc_url: &str,
    vault: Address,
    signer: &CouponSigner,
) -> Result<()> {
    tokio::time::timeout(timeout, read_and_compare(network, rpc_url, vault, signer))
        .await
        .map_err(|_| {
            anyhow!(
                "{} did not answer within {timeout:?}",
                network.rpc_url_var()
            )
        })?
}

async fn read_and_compare(
    network: Network,
    rpc_url: &str,
    vault: Address,
    signer: &CouponSigner,
) -> Result<()> {
    let order = signer.order();
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
    if order_hash(&claim_order(order)) != order.order_hash {
        faults.push(
            "ORDER_HASH is not the hash of the order that ORDER_OWNER, TOKEN_ADDRESS, VAULT_ID, CLAIM_INTERPRETER, CLAIM_STORE, CLAIM_EXPRESSION and CLAIM_INPUT_TOKEN describe"
                .to_string(),
        );
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

    let signer_code = provider
        .get_code_at(order.signer)
        .await
        .map_err(|e| read_failed("eth_getCode(CLAIM_SIGNER)", e))?;
    if !signer_code.is_empty() && !is_delegated_key(&signer_code) {
        bail!(
            "CLAIM_SIGNER {} is a contract, such as a Safe: the orchestrator signs coupons only with SIGNER_PRIVATE_KEY, a key, so it cannot sign for it",
            order.signer
        );
    }
    if signer.address() != order.signer {
        bail!(
            "SIGNER_PRIVATE_KEY is the key of {}, not of CLAIM_SIGNER {}",
            signer.address(),
            order.signer
        );
    }

    let probe = signer.probe_coupon().await?;
    let taken = IOrderBookV3::new(order.orderbook, &provider)
        .takeOrders(take_one(order, probe))
        .from(order.signer)
        .call()
        .await;
    accepts_signer(order.signer, taken.map(|_| ()), rpc_var)
}

/// Whether simulating takeOrders with the probe coupon shows the claim order accepts
/// its signer. Its first check is the signer, so any other outcome comes after it.
/// Only a claim, or MinimumInput for a vault that cannot pay the one wei, counts.
fn accepts_signer(
    signer: Address,
    taken: std::result::Result<(), alloy::contract::Error>,
    rpc_var: &str,
) -> Result<()> {
    let what = "the claim order probe (takeOrders)";
    let payload = match taken {
        Ok(()) => return Ok(()),
        Err(alloy::contract::Error::TransportError(RpcError::ErrorResp(payload))) => payload,
        Err(alloy::contract::Error::TransportError(e)) => {
            return Err(rpc_failure(rpc_var, what, &e))
        }
        Err(_) => bail!("{what} answered with data that is not takeOrders' result"),
    };
    let Some(revert) = payload.as_revert_data() else {
        return Err(rpc_failure(rpc_var, what, &RpcError::ErrorResp(payload)));
    };
    if MinimumInput::abi_decode(&revert, true).is_ok() {
        return Ok(());
    }
    if Revert::abi_decode(&revert, true).is_ok_and(|r| r.reason == "Wrong signer") {
        bail!("the claim order ORDER_HASH does not accept coupons from CLAIM_SIGNER {signer}: it answered \"Wrong signer\"");
    }
    bail!(
        "the claim order ORDER_HASH refused a coupon from CLAIM_SIGNER {signer} with error 0x{}",
        hex::encode(&revert[..revert.len().min(4)])
    )
}

/// An EIP-7702 delegation leaves an account a key, which the orderbook checks by its
/// ECDSA signature before it asks any code.
fn is_delegated_key(code: &Bytes) -> bool {
    code.len() == 23 && code.starts_with(&[0xef, 0x01, 0x00])
}

/// The OrderV2 the vault added, as script/ClaimOrderScript.sol adds it.
fn claim_order(order: &ClaimOrder) -> OrderV2 {
    let io = |token| IO {
        token,
        decimals: 18,
        vaultId: order.vault_id,
    };
    OrderV2 {
        owner: order.order_owner,
        handleIO: true,
        evaluable: EvaluableV2 {
            interpreter: order.interpreter,
            store: order.store,
            expression: order.expression,
        },
        validInputs: vec![io(order.input_token)],
        validOutputs: vec![io(order.token)],
    }
}

fn order_hash(order: &OrderV2) -> B256 {
    keccak256(order.abi_encode())
}

fn take_one(order: &ClaimOrder, probe: SignedCoupon) -> TakeOrdersConfigV2 {
    let amount = probe.context[1];
    TakeOrdersConfigV2 {
        minimumInput: amount,
        maximumInput: amount,
        maximumIORatio: U256::ZERO,
        orders: vec![TakeOrderConfigV2 {
            order: claim_order(order),
            inputIOIndex: U256::ZERO,
            outputIOIndex: U256::ZERO,
            signedContext: vec![SignedContextV1 {
                signer: probe.signer,
                context: probe.context,
                signature: probe.signature.into(),
            }],
        }],
        data: Bytes::new(),
    }
}

/// The cause of a failed read, in words of our own: a provider's URL can hold its
/// API key, reqwest names the URL in its message, and the provider's own answer can
/// echo either. Only reqwest's underlying causes, the HTTP status and the JSON-RPC
/// error code are kept.
pub(crate) fn rpc_failure(rpc_var: &str, what: &str, err: &TransportError) -> anyhow::Error {
    let cause = match err {
        RpcError::Transport(TransportErrorKind::Custom(inner)) => {
            match inner.downcast_ref::<reqwest::Error>() {
                Some(request) => causes(request),
                None => "the transport failed".to_string(),
            }
        }
        RpcError::Transport(TransportErrorKind::HttpError(http)) => {
            format!("the RPC answered HTTP {}", http.status)
        }
        RpcError::Transport(_) => "the transport failed".to_string(),
        RpcError::ErrorResp(payload) => {
            format!("the RPC answered with error code {}", payload.code)
        }
        RpcError::NullResp => "the RPC answered with no result".to_string(),
        RpcError::DeserError { .. } => "the RPC's answer could not be read".to_string(),
        RpcError::SerError(_) | RpcError::LocalUsageError(_) | RpcError::UnsupportedFeature(_) => {
            "the request could not be made".to_string()
        }
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
    use crate::fake_rpc::{read_request, serve};
    use alloy::primitives::{address, PrimitiveSignature};
    use alloy::signers::local::PrivateKeySigner;
    use alloy::sol_types::SolCall;
    use serde_json::{json, Value};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    const VAULT: Address = address!("E3E064e3C2EEf66cb93dA8D8114F5084E92F48D6");
    const HOT: Address = address!("6c6EE5e31d828De241282B9606C8e98Ea48526E2");
    const ORDERBOOK: Address = address!("f1224A483ad7F1E9aA46A8CE41229F32d7549A74");
    const OTHER: Address = address!("1111111111111111111111111111111111111111");
    const KEY: &str = "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";

    fn vault_id() -> U256 {
        "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b"
            .parse()
            .unwrap()
    }

    fn key() -> PrivateKeySigner {
        KEY.parse().unwrap()
    }

    /// A claim order whose signer is `key()`, its hash rebuilt from its values.
    fn order() -> ClaimOrder {
        let mut order = ClaimOrder {
            order_hash: B256::ZERO,
            order_owner: VAULT,
            orderbook: ORDERBOOK,
            token: HOT,
            vault_id: vault_id(),
            signer: key().address(),
            interpreter: address!("4C7436641da0505A8012218c1524Db0060Fd7253"),
            store: address!("32a868432101C516647E7Ee217CA641B288953C6"),
            expression: address!("1e814F560938B7Ed82Ba00Cc075a822E4789309E"),
            input_token: address!("dAC17F958D2ee523a2206206994597C13D831ec7"),
        };
        order.order_hash = order_hash(&claim_order(&order));
        order
    }

    fn signer_for(order: ClaimOrder) -> CouponSigner {
        CouponSigner::for_order(key(), order)
    }

    /// What a JSON-RPC endpoint answers about one chain: the vault deployed with
    /// `token`, `orderbook` and `vault_id`, the orders that orderbook holds, and the
    /// signer the claim order accepts.
    #[derive(Clone)]
    struct Chain {
        chain_id: u64,
        vault_deployed: bool,
        token: Address,
        orderbook: Address,
        vault_id: U256,
        orders: Vec<B256>,
        valid_signer: Address,
        signer_code: &'static str,
        vault_balance: U256,
    }

    type Answer = std::result::Result<String, Value>;

    fn reverted(data: Vec<u8>) -> Answer {
        Err(
            json!({"code": 3, "message": "execution reverted", "data": format!("0x{}", hex::encode(data))}),
        )
    }

    impl Chain {
        fn of(network: Network) -> Self {
            Self {
                chain_id: network.chain_id(),
                vault_deployed: true,
                token: HOT,
                orderbook: ORDERBOOK,
                vault_id: vault_id(),
                orders: vec![order().order_hash],
                valid_signer: key().address(),
                signer_code: "0x",
                vault_balance: U256::from(10),
            }
        }

        fn answer(&self, method: &str, params: &Value) -> Answer {
            let empty = Ok("0x".to_string());
            match method {
                "eth_chainId" => Ok(format!("{:#x}", self.chain_id)),
                "eth_getCode" if self.vault_deployed && address_at(&params[0]) == VAULT => {
                    Ok("0x6080".to_string())
                }
                "eth_getCode" if address_at(&params[0]) != VAULT => {
                    Ok(self.signer_code.to_string())
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
                    } else if to == self.orderbook
                        && selector == IOrderBookV3::takeOrdersCall::SELECTOR
                    {
                        return self.take_orders(&input);
                    } else {
                        return empty;
                    };
                    Ok(format!("0x{}", hex::encode(encoded)))
                }
                other => panic!("the check sent {other}"),
            }
        }

        /// takeOrders as OrderBookV3 runs the claim order: the coupon's signature
        /// first, then the expression's signer check, then the vault's balance.
        fn take_orders(&self, input: &[u8]) -> Answer {
            let config = IOrderBookV3::takeOrdersCall::abi_decode(input, true)
                .unwrap()
                .config;
            let take = &config.orders[0];
            let coupon = &take.signedContext[0];
            let packed: Vec<u8> = coupon
                .context
                .iter()
                .flat_map(|v| v.to_be_bytes::<32>())
                .collect();
            let digest = keccak256(
                [
                    b"\x19Ethereum Signed Message:\n32".as_slice(),
                    keccak256(&packed).as_slice(),
                ]
                .concat(),
            );
            let recovered = PrimitiveSignature::try_from(coupon.signature.as_ref())
                .unwrap()
                .recover_address_from_prehash(&digest)
                .unwrap();
            assert_eq!(recovered, coupon.signer, "the probe's signature");
            assert!(
                self.orders.contains(&order_hash(&take.order)),
                "the probe named an order the orderbook does not hold"
            );
            if coupon.signer != self.valid_signer {
                return reverted(
                    Revert {
                        reason: "Wrong signer".to_string(),
                    }
                    .abi_encode(),
                );
            }
            if self.vault_balance < config.minimumInput {
                return reverted(
                    MinimumInput {
                        minimumInput: config.minimumInput,
                        input: U256::ZERO,
                    }
                    .abi_encode(),
                );
            }
            Ok(format!(
                "0x{}",
                hex::encode((config.minimumInput, U256::ZERO).abi_encode_params())
            ))
        }
    }

    fn address_at(value: &Value) -> Address {
        value.as_str().unwrap().parse().unwrap()
    }

    /// Answers every request with `status` and `body`, whatever it asks.
    async fn serve_answer(status: &'static str, body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v3/{SECRET}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                read_request(&mut socket).await;
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        url
    }

    const SECRET: &str = "secret-api-key";

    #[tokio::test]
    async fn names_a_failed_answer_in_words_of_its_own() {
        let leaky = format!("https://rpc.example/v3/{SECRET} refused the body {SECRET}");
        let answers = [
            (
                "200 OK",
                json!({"jsonrpc": "2.0", "id": 0, "error": {"code": -32000, "message": leaky, "data": leaky}}).to_string(),
                "ETH_RPC_URL: eth_chainId failed: the RPC answered with error code -32000",
            ),
            (
                "401 Unauthorized",
                leaky.clone(),
                "ETH_RPC_URL: eth_chainId failed: the RPC answered HTTP 401",
            ),
            (
                "200 OK",
                leaky.clone(),
                "ETH_RPC_URL: eth_chainId failed: the RPC's answer could not be read",
            ),
        ];
        for (status, body, expected) in answers {
            let url = serve_answer(status, body).await;

            let err = check(Network::Mainnet, &url, VAULT, &signer_for(order()))
                .await
                .expect_err("a failed answer passed the check");

            assert_eq!(format!("{err:#}"), expected);
        }
    }

    async fn check_against(network: Network, chain: Chain, order: &ClaimOrder) -> Result<()> {
        let url =
            serve(move |method, params| chain.answer(method, params).map(Value::String)).await;
        check(network, &url, VAULT, &signer_for(order.clone())).await
    }

    /// `order` changed by `change`, with its hash rebuilt, and a chain that holds it.
    fn changed(change: impl FnOnce(&mut ClaimOrder)) -> (ClaimOrder, Chain) {
        let mut order = order();
        change(&mut order);
        order.order_hash = order_hash(&claim_order(&order));
        let chain = Chain {
            orders: vec![order.order_hash],
            ..Chain::of(Network::Sepolia)
        };
        (order, chain)
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
        let (order, chain) = changed(|order| order.order_owner = OTHER);

        assert_eq!(
            refusal(Network::Sepolia, chain, &order).await,
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
            format!(
                "ORDER_HASH {} is not an order on ORDERBOOK_ADDRESS {ORDERBOOK}",
                order().order_hash
            )
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
    async fn refuses_an_rpc_that_never_answers() {
        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", silent.local_addr().unwrap());
        let held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((socket, _)) = silent.accept().await {
                open.push(socket);
            }
        });

        let err = tokio::time::timeout(
            Duration::from_secs(10),
            check_within(
                Duration::from_millis(200),
                Network::Sepolia,
                &url,
                VAULT,
                &signer_for(order()),
            ),
        )
        .await
        .expect("the check waited on a silent RPC with no end")
        .expect_err("a silent RPC passed the check");
        held.abort();

        assert_eq!(
            format!("{err:#}"),
            "SEPOLIA_RPC_URL did not answer within 200ms"
        );
    }

    #[tokio::test]
    async fn refuses_an_rpc_it_cannot_reach_without_naming_its_url() {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v3/secret-api-key", closed.local_addr().unwrap());
        drop(closed);

        let err = check(Network::Mainnet, &url, VAULT, &signer_for(order()))
            .await
            .expect_err("an unreachable RPC passed the check");
        let message = format!("{err:#}");

        assert!(
            message.starts_with("ETH_RPC_URL: eth_chainId failed: "),
            "{message}"
        );
        assert!(!message.contains("secret-api-key"), "{message}");
    }

    #[tokio::test]
    async fn refuses_order_values_that_do_not_hash_to_order_hash() {
        let order = ClaimOrder {
            store: OTHER,
            ..order()
        };

        assert_eq!(
            refusal(Network::Mainnet, Chain::of(Network::Mainnet), &order).await,
            "ORDER_HASH is not the hash of the order that ORDER_OWNER, TOKEN_ADDRESS, VAULT_ID, CLAIM_INTERPRETER, CLAIM_STORE, CLAIM_EXPRESSION and CLAIM_INPUT_TOKEN describe"
        );
    }

    #[test]
    fn rebuilds_the_hash_of_the_claim_order_on_sepolia() {
        let sepolia = ClaimOrder::sepolia();

        assert_eq!(order_hash(&claim_order(&sepolia)), sepolia.order_hash);
    }

    #[tokio::test]
    async fn refuses_a_key_the_claim_order_does_not_accept() {
        for network in [Network::Sepolia, Network::Mainnet] {
            let chain = Chain {
                valid_signer: OTHER,
                ..Chain::of(network)
            };

            assert_eq!(
                refusal(network, chain, &order()).await,
                format!(
                    "the claim order ORDER_HASH does not accept coupons from CLAIM_SIGNER {}: it answered \"Wrong signer\"",
                    key().address()
                )
            );
        }
    }

    #[tokio::test]
    async fn passes_a_claim_order_that_accepts_the_key_while_its_vault_is_empty() {
        let chain = Chain {
            vault_balance: U256::ZERO,
            ..Chain::of(Network::Mainnet)
        };

        check_against(Network::Mainnet, chain, &order())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn refuses_a_claim_signer_that_is_a_contract() {
        for network in [Network::Sepolia, Network::Mainnet] {
            let chain = Chain {
                signer_code: "0x608060405260043610",
                ..Chain::of(network)
            };

            assert_eq!(
                refusal(network, chain, &order()).await,
                format!(
                    "CLAIM_SIGNER {} is a contract, such as a Safe: the orchestrator signs coupons only with SIGNER_PRIVATE_KEY, a key, so it cannot sign for it",
                    key().address()
                )
            );
        }
    }

    #[tokio::test]
    async fn refuses_a_safe_as_the_claim_signer_whatever_the_key() {
        let (order, chain) = changed(|order| order.signer = OTHER);
        let chain = Chain {
            signer_code: "0x608060405260043610",
            ..chain
        };

        assert_eq!(
            refusal(Network::Sepolia, chain, &order).await,
            format!(
                "CLAIM_SIGNER {OTHER} is a contract, such as a Safe: the orchestrator signs coupons only with SIGNER_PRIVATE_KEY, a key, so it cannot sign for it"
            )
        );
    }

    #[tokio::test]
    async fn refuses_a_key_that_is_not_the_claim_signer() {
        let (order, chain) = changed(|order| order.signer = OTHER);

        assert_eq!(
            refusal(Network::Sepolia, chain, &order).await,
            format!(
                "SIGNER_PRIVATE_KEY is the key of {}, not of CLAIM_SIGNER {OTHER}",
                key().address()
            )
        );
    }

    #[tokio::test]
    async fn passes_a_claim_signer_that_is_a_key_with_an_eip_7702_delegation() {
        let chain = Chain {
            signer_code: "0xef01001111111111111111111111111111111111111111",
            ..Chain::of(Network::Mainnet)
        };

        check_against(Network::Mainnet, chain, &order())
            .await
            .unwrap();
    }

    #[test]
    fn refuses_any_other_answer_to_the_probe() {
        let other = |data: Vec<u8>| {
            let answer = json!({"code": 3, "message": "execution reverted", "data": format!("0x{}", hex::encode(data))});
            Err(alloy::contract::Error::TransportError(RpcError::ErrorResp(
                serde_json::from_value(answer).unwrap(),
            )))
        };
        let refused = |taken| {
            format!(
                "{:#}",
                accepts_signer(OTHER, taken, "ETH_RPC_URL").unwrap_err()
            )
        };

        assert_eq!(
            refused(other(Revert { reason: "Order expired".to_string() }.abi_encode())),
            format!("the claim order ORDER_HASH refused a coupon from CLAIM_SIGNER {OTHER} with error 0x08c379a0")
        );
        assert_eq!(
            refused(other(vec![])),
            format!("the claim order ORDER_HASH refused a coupon from CLAIM_SIGNER {OTHER} with error 0x")
        );
    }
}
