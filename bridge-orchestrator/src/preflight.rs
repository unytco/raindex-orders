use crate::config::Network;
use crate::signer::{
    disjoint_quorums, safe_domain_separator, ClaimOrder, CouponSigner, ISafe, Owner, SafeOwners,
    SignedCoupon, ONE_PARTY_OVERRIDE,
};
use alloy::primitives::{keccak256, Address, Bytes, B256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::sol;
use alloy::sol_types::{Revert, SolCall, SolError, SolValue};
use alloy::transports::http::{Client, Http};
use alloy::transports::{RpcError, TransportError, TransportErrorKind};
use anyhow::{anyhow, bail, Result};
use std::error::Error;
use std::time::Duration;

sol! {
    struct IOV2 {
        address token;
        bytes32 vaultId;
    }

    struct EvaluableV4 {
        address interpreter;
        address store;
        bytes bytecode;
    }

    struct OrderV4 {
        address owner;
        EvaluableV4 evaluable;
        IOV2[] validInputs;
        IOV2[] validOutputs;
        bytes32 nonce;
    }

    struct SignedContextV1 {
        address signer;
        bytes32[] context;
        bytes signature;
    }

    struct TakeOrderConfigV4 {
        OrderV4 order;
        uint256 inputIOIndex;
        uint256 outputIOIndex;
        SignedContextV1[] signedContext;
    }

    /// Every amount is a decimal float, a bytes32.
    struct TakeOrdersConfigV5 {
        bytes32 minimumIO;
        bytes32 maximumIO;
        bytes32 maximumIORatio;
        bool IOIsInput;
        TakeOrderConfigV4[] orders;
        bytes data;
    }

    error MinimumIO(bytes32 minimumIO, bytes32 actualIO);

    /// Raindex V6's refusal of signed context `i` whose signature its signer
    /// does not accept (rainlang-interface's LibContext). The claim's coupon is
    /// signed context 0.
    error InvalidSignature(uint256 i);

    #[sol(rpc)]
    interface IHoloLockVault {
        function token() external view returns (address);
        function orderbook() external view returns (address);
        function vaultId() external view returns (bytes32);
        function admin() external view returns (address);
    }

    #[sol(rpc)]
    interface IRaindexV6 {
        function orderExists(bytes32 orderHash) external view returns (bool);
        function takeOrders4(TakeOrdersConfigV5 calldata config)
            external
            returns (bytes32 totalTakerInput, bytes32 totalTakerOutput);
    }
}

pub const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// The claim order's io-ratio, `1e-30` in src/holo-claim.rain, as the float the
/// parser makes of it: coefficient 1, exponent -30 (`CLAIM_IO_RATIO` in
/// src/Constants.sol). A claim passes it as its maximum ratio.
pub const CLAIM_IO_RATIO: B256 = B256::new([
    0xff, 0xff, 0xff, 0xe2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 1,
]);

/// The claim order's nonce, `CLAIM_ORDER_NONCE` in src/Constants.sol.
pub const CLAIM_ORDER_NONCE: B256 = B256::ZERO;

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

/// The check, each run of chain reads within `timeout`: the reads of the
/// chain, the vault, the claim order and the Safe, and then the probe's
/// simulations. The signer's own waits between them are not charged to the
/// RPC: each service's `GET /signer` (IDENTITY_TIMEOUT), the probe's
/// signatures (SIGN_TIMEOUT) and the Safe's reads for them (the signer's own
/// RPC_TIMEOUT each), so a slow signer service is not reported as the RPC not
/// answering.
async fn check_within(
    timeout: Duration,
    network: Network,
    rpc_url: &str,
    vault: Address,
    signer: &CouponSigner,
) -> Result<()> {
    let rpc_var = network.rpc_url_var();
    let (provider, safe) = rpc_within(
        timeout,
        rpc_var,
        read_and_compare(network, rpc_url, vault, signer),
    )
    .await?;
    if let (Some(owners), Some(safe)) = (signer.safe(), safe) {
        check_safe_owners(network, owners, &safe).await?;
    }
    let probe = signer.probe_coupon().await?;
    rpc_within(
        timeout,
        rpc_var,
        simulate_probe(
            &provider,
            signer.order(),
            probe,
            signer.safe().is_some(),
            rpc_var,
        ),
    )
    .await
}

/// `read`, chain reads through the RPC, within `timeout`.
async fn rpc_within<T>(
    timeout: Duration,
    rpc_var: &str,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(timeout, read)
        .await
        .map_err(|_| anyhow!("{rpc_var} did not answer within {timeout:?}"))?
}

/// What the chain answered about CLAIM_SIGNER, a Safe: its threshold and
/// owners, and the vault's admin.
struct SafeRead {
    threshold: usize,
    owners: Vec<Address>,
    admin: Address,
}

/// The chain reads: the RPC's chain, the vault and the claim order compared
/// with the configuration, and, for a Safe signer, the Safe's state. Answers
/// the provider they went through, for the probe's simulations.
async fn read_and_compare(
    network: Network,
    rpc_url: &str,
    vault: Address,
    signer: &CouponSigner,
) -> Result<(RootProvider<Http<Client>>, Option<SafeRead>)> {
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
            "VAULT_ID {} is not the vault's vaultId {vault_id}",
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
            "ORDER_HASH is not the hash of the order that ORDER_OWNER, TOKEN_ADDRESS, VAULT_ID, CLAIM_INTERPRETER, CLAIM_STORE, CLAIM_BYTECODE and CLAIM_INPUT_TOKEN describe"
                .to_string(),
        );
    }
    if orderbook != order.orderbook {
        faults.push(format!(
            "ORDERBOOK_ADDRESS {} is not the vault's orderbook {orderbook}",
            order.orderbook
        ));
    } else {
        let exists = IRaindexV6::new(orderbook, &provider)
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
    if is_delegated_key(&signer_code) {
        bail!(
            "CLAIM_SIGNER {} has an EIP-7702 delegation: Raindex V6 asks the delegate contract for a coupon's signature in place of the key, so the key's coupons are refused. Use a key with no delegation",
            order.signer
        );
    }
    let safe = match signer.safe() {
        None => {
            if !signer_code.is_empty() {
                bail!(
                    "CLAIM_SIGNER {} is a contract, such as a Safe, and SIGNER_PRIVATE_KEY signs as a key, so it cannot sign for it. For a Safe, set SAFE_OWNER_KEYS or SAFE_OWNER_SERVICES to its owners instead",
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
            None
        }
        Some(safe) => {
            if signer_code.is_empty() {
                bail!(
                    "CLAIM_SIGNER {} has no contract, so it is a key, not a Safe, and SAFE_OWNER_KEYS and SAFE_OWNER_SERVICES sign for a Safe. For a key, set SIGNER_PRIVATE_KEY instead",
                    order.signer
                );
            }
            let admin = lock_vault
                .admin()
                .call()
                .await
                .map_err(|e| call_failed("admin()", e))?
                ._0;
            Some(check_safe(&provider, network, safe, admin, rpc_var).await?)
        }
    };
    Ok((provider, safe))
}

/// The probe, a one-wei coupon to the signer, simulated as the website sends
/// it: the claim order must take it, and, for a Safe (`safe`), refuse it with
/// one owner signature fewer than its threshold.
async fn simulate_probe<T, P>(
    provider: &P,
    order: &ClaimOrder,
    probe: SignedCoupon,
    safe: bool,
    rpc_var: &str,
) -> Result<()>
where
    T: alloy::transports::Transport + Clone,
    P: Provider<T>,
{
    let fewer = safe.then(|| {
        let mut fewer = probe.clone();
        fewer.signature.truncate(probe.signature.len() - 65);
        fewer
    });
    let refused = simulate(provider, order, probe, rpc_var).await?;
    accepts_signer(order.signer, refused)?;
    if let Some(fewer) = fewer {
        let owners = fewer.signature.len() / 65;
        let Some(revert) = simulate(provider, order, fewer, rpc_var).await? else {
            bail!(
                "the claim order ORDER_HASH took a coupon carrying {owners} owner signatures for CLAIM_SIGNER {}, fewer than its threshold",
                order.signer
            );
        };
        // Only the Safe refusing the signatures shows it takes no fewer than its
        // threshold: any other revert would pass this probe for another reason.
        if !InvalidSignature::abi_decode(&revert, true).is_ok_and(|e| e.i.is_zero()) {
            bail!(
                "the claim order ORDER_HASH refused a coupon carrying {owners} owner signatures for CLAIM_SIGNER {} with {}, not InvalidSignature(0): that does not show the Safe refuses fewer than its threshold",
                order.signer,
                revert_name(&revert)
            );
        }
        tracing::info!(
            event = "startup.probe",
            "the claim order took the probe signed by {} owners of CLAIM_SIGNER {}, and refused it signed by {owners}",
            owners + 1,
            order.signer
        );
    }
    Ok(())
}

/// Simulates the claim of `coupon` as the website sends it. Nothing when the
/// claim order took it, or would have but for the vault's balance (MinimumIO);
/// otherwise its revert data.
async fn simulate<T, P>(
    provider: &P,
    order: &ClaimOrder,
    coupon: SignedCoupon,
    rpc_var: &str,
) -> Result<Option<Bytes>>
where
    T: alloy::transports::Transport + Clone,
    P: Provider<T>,
{
    let call = take_one(order, coupon);
    tracing::debug!(
        event = "startup.probe",
        calldata = %Bytes::from(IRaindexV6::takeOrders4Call { config: call.clone() }.abi_encode()),
        "simulating the claim order probe"
    );
    let taken = IRaindexV6::new(order.orderbook, provider)
        .takeOrders4(call)
        .from(order.signer)
        .call()
        .await;
    outcome(taken.map(|_| ()), rpc_var)
}

/// CLAIM_SIGNER, a Safe, must be one on `network`'s chain whose EIP-712 domain
/// is the one its owners sign under, or its `isValidSignature` refuses every
/// coupon. Its threshold must be more than half its owners, so that every two
/// sets of signers share an owner (`disjoint_quorums`), on any network and with
/// UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes too. Each owner key held here must be one of
/// its owners, and no more of them than its threshold. What it read, with the
/// vault's `admin`, is for `check_safe_owners`, which asks the signer services.
async fn check_safe<T, P>(
    provider: &P,
    network: Network,
    signer: &SafeOwners,
    admin: Address,
    rpc_var: &str,
) -> Result<SafeRead>
where
    T: alloy::transports::Transport + Clone,
    P: Provider<T>,
{
    let safe = signer.safe();
    let contract = ISafe::new(safe, provider);
    let not_a_safe = |what: &str, err: alloy::contract::Error| match err {
        alloy::contract::Error::TransportError(e) => rpc_failure(rpc_var, what, &e),
        _ => anyhow!("CLAIM_SIGNER {safe} is a contract but not a Safe: it did not answer {what}"),
    };
    let threshold = contract
        .getThreshold()
        .call()
        .await
        .map_err(|e| not_a_safe("getThreshold()", e))?
        ._0;
    let owners = contract
        .getOwners()
        .call()
        .await
        .map_err(|e| not_a_safe("getOwners()", e))?
        ._0;
    let domain = contract
        .domainSeparator()
        .call()
        .await
        .map_err(|e| not_a_safe("domainSeparator()", e))?
        ._0;
    let chain_id = network.chain_id();
    if domain != safe_domain_separator(chain_id, safe) {
        bail!(
            "CLAIM_SIGNER {safe} does not sign under the EIP-712 domain of a Safe 1.3.0 or later on chain {chain_id}, so its owners' signatures would not be over the hash it checks"
        );
    }
    let threshold = usize::try_from(threshold).unwrap_or(usize::MAX);
    if let Some(fault) = disjoint_quorums(safe, threshold, owners.len()) {
        bail!("{fault}");
    }
    let listed = owners
        .iter()
        .map(|owner| owner.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let keys = signer.local_keys();
    if keys > threshold {
        bail!(
            "SAFE_OWNER_KEYS holds {keys} keys, and CLAIM_SIGNER {safe} is a Safe of threshold {threshold}: give the keys of that many of its owners"
        );
    }
    for owner in signer.owners() {
        let address = owner.address();
        if !owners.contains(&address) {
            bail!(
                "{} {address}, which is not an owner of CLAIM_SIGNER {safe}: its owners are {listed}",
                match owner {
                    Owner::Key(_) => "SAFE_OWNER_KEYS holds the key of",
                    Owner::Service(_) => "SAFE_OWNER_SERVICES names",
                }
            );
        }
    }
    Ok(SafeRead {
        threshold,
        owners,
        admin,
    })
}

/// Each signer service must answer for its owner, this Safe and this chain.
/// The services are asked at once, each within IDENTITY_TIMEOUT. A service
/// that does not answer is logged and left out of the probe, and the check
/// passes while the owners that can sign are at least the threshold: a
/// withdrawal stays parked while fewer sign. No RPC read: the Safe's state is
/// `read`.
///
/// On mainnet it also refuses, unless UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes, a Safe
/// this process alone could sign as: one of threshold 1, or one whose threshold
/// of owner keys it holds. And it refuses any owner key held here when the Safe
/// is also the vault `admin`, as that key also approves `adminWithdraw`.
async fn check_safe_owners(network: Network, signer: &SafeOwners, read: &SafeRead) -> Result<()> {
    let safe = signer.safe();
    let chain_id = network.chain_id();
    let SafeRead {
        threshold, admin, ..
    } = *read;
    let listed = read
        .owners
        .iter()
        .map(|owner| owner.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let keys = signer.local_keys();
    // Every service at once: one that does not answer costs one
    // IDENTITY_TIMEOUT, not one for each service after it.
    let identities = futures::future::join_all(signer.owners().iter().map(|owner| async move {
        match owner {
            Owner::Key(_) => None,
            Owner::Service(service) => Some(service.identity().await),
        }
    }))
    .await;
    let mut able = Vec::new();
    for (owner, identity) in signer.owners().iter().zip(identities) {
        let address = owner.address();
        let (Owner::Service(service), Some(identity)) = (owner, identity) else {
            able.push(format!("{address}, a key held here"));
            continue;
        };
        match identity {
            Ok(identity) => {
                if identity.owner != address
                    || identity.safe != safe
                    || identity.chain_id != chain_id
                {
                    bail!(
                        "the signer service at {} signs as {} for {} on chain {}, and SAFE_OWNER_SERVICES names it for {address}, an owner of CLAIM_SIGNER {safe} on chain {chain_id}",
                        service.host(),
                        identity.owner,
                        identity.safe,
                        identity.chain_id
                    );
                }
                able.push(format!("{address}, a signer service at {}", service.host()));
            }
            Err(err) => {
                tracing::warn!(
                    event = "startup.signer_service_down",
                    owner = %address,
                    "the signer service for {address} at {} {err:#}: the other owners sign until it answers",
                    service.host()
                );
                // Not asked to sign the probe; the first cycle asks it again.
                signer.leave_out(address, format!("it {err:#} at startup"));
            }
        }
    }
    if able.len() < threshold {
        bail!(
            "{} of the owners this orchestrator asks can sign now, and CLAIM_SIGNER {safe} is a Safe of threshold {threshold}: {}",
            able.len(),
            if able.is_empty() {
                "none".to_string()
            } else {
                able.join("; ")
            }
        );
    }
    tracing::info!(
        event = "startup.safe_signer",
        safe = %safe,
        threshold,
        owners = %listed,
        "CLAIM_SIGNER is a Safe of threshold {threshold} with the owners {listed}. This orchestrator asks {}: {}",
        signer.owners().len(),
        able.join("; ")
    );
    let mut one_party = Vec::new();
    if threshold < 2 {
        one_party.push(format!(
            "CLAIM_SIGNER {safe} is a Safe of threshold {threshold}, which one owner alone signs as"
        ));
    }
    if keys >= threshold {
        one_party.push(format!(
            "this orchestrator holds the keys of {keys} owners of CLAIM_SIGNER {safe}, a Safe of threshold {threshold}, and signs as all of them, which is one party, not {threshold}"
        ));
    }
    if keys > 0 && admin == safe {
        one_party.push(format!(
            "CLAIM_SIGNER {safe} is also the vault's admin, so the {keys} owner keys in SAFE_OWNER_KEYS also approve its admin transactions, adminWithdraw among them"
        ));
    }
    if one_party.is_empty() {
        return Ok(());
    }
    let one_party = one_party.join("; ");
    if network == Network::Mainnet && !signer.one_party_allowed() {
        bail!(
            "{one_party}. Mainnet takes owners at independent signer services, SAFE_OWNER_SERVICES (docs/signer-service.md). To start regardless, set {ONE_PARTY_OVERRIDE}=yes"
        );
    }
    tracing::warn!(
        event = "startup.one_party",
        "{one_party} (docs/signer-service.md){}",
        if network == Network::Mainnet {
            format!(": {ONE_PARTY_OVERRIDE}=yes")
        } else {
            String::new()
        }
    );
    Ok(())
}

/// What a simulated claim answered: nothing when the claim order took it, or
/// would have but for the vault's balance (MinimumIO); otherwise its revert
/// data. A transport failure, or an answer that is not takeOrders4's, is an error.
fn outcome(
    taken: std::result::Result<(), alloy::contract::Error>,
    rpc_var: &str,
) -> Result<Option<Bytes>> {
    let what = "the claim order probe (takeOrders4)";
    let payload = match taken {
        Ok(()) => return Ok(None),
        Err(alloy::contract::Error::TransportError(RpcError::ErrorResp(payload))) => payload,
        Err(alloy::contract::Error::TransportError(e)) => {
            return Err(rpc_failure(rpc_var, what, &e))
        }
        Err(_) => bail!("{what} answered with data that is not takeOrders4's result"),
    };
    let Some(revert) = payload.as_revert_data() else {
        return Err(rpc_failure(rpc_var, what, &RpcError::ErrorResp(payload)));
    };
    if MinimumIO::abi_decode(&revert, true).is_ok() {
        return Ok(None);
    }
    Ok(Some(revert))
}

/// The claim order's refusal of a coupon whose nonce was claimed before.
const NONCE_USED: &str = "Nonce already used";

/// Whether the simulated probe shows the claim order accepts its signer. Its
/// first check is the signer, so any other outcome comes after it. A claim, or
/// MinimumIO for a vault that cannot pay the one wei, counts. So does "Nonce
/// already used": Raindex checks the coupon's signature before the expression
/// runs, and the nonce is the expression's last check (src/holo-claim.rain),
/// after the signer and every other word, so the order took all of those. The
/// probe pays the signer itself, so only the signer can have claimed it, once;
/// its nonce is fixed, and refusing it would stop every later startup.
fn accepts_signer(signer: Address, refused: Option<Bytes>) -> Result<()> {
    let Some(revert) = refused else {
        return Ok(());
    };
    let reason = Revert::abi_decode(&revert, true).map(|r| r.reason);
    if reason.as_deref().is_ok_and(|r| r == "Wrong signer") {
        bail!("the claim order ORDER_HASH does not accept coupons from CLAIM_SIGNER {signer}: it answered \"Wrong signer\"");
    }
    if reason.as_deref().is_ok_and(|r| r == NONCE_USED) {
        tracing::info!(
            event = "startup.probe",
            "the claim order answered \"{NONCE_USED}\" to the probe, its last check: CLAIM_SIGNER {signer} claimed the probe once, and the order accepts its coupons"
        );
        return Ok(());
    }
    bail!(
        "the claim order ORDER_HASH refused a coupon from CLAIM_SIGNER {signer} with error 0x{}",
        hex::encode(&revert[..revert.len().min(4)])
    )
}

/// A revert, for logs: its reason when it is a `require` string, else its
/// error selector.
fn revert_name(revert: &[u8]) -> String {
    match Revert::abi_decode(revert, true) {
        Ok(r) => format!("\"{}\"", r.reason.chars().take(100).collect::<String>()),
        Err(_) => format!("error 0x{}", hex::encode(&revert[..revert.len().min(4)])),
    }
}

/// An account with an EIP-7702 delegation. OrderBookV3 checked such a signer's
/// coupons by their ECDSA signature; Raindex V6 (OpenZeppelin 5's SignatureChecker)
/// asks any account with code, a delegation included, through EIP-1271 only, so a
/// delegated key's coupons are refused.
fn is_delegated_key(code: &Bytes) -> bool {
    code.len() == 23 && code.starts_with(&[0xef, 0x01, 0x00])
}

/// The OrderV4 the vault added, as script/ClaimOrderScript.sol adds it.
fn claim_order(order: &ClaimOrder) -> OrderV4 {
    let io = |token| IOV2 {
        token,
        vaultId: order.vault_id,
    };
    OrderV4 {
        owner: order.order_owner,
        evaluable: EvaluableV4 {
            interpreter: order.interpreter,
            store: order.store,
            bytecode: order.bytecode.clone(),
        },
        validInputs: vec![io(order.input_token)],
        validOutputs: vec![io(order.token)],
        nonce: CLAIM_ORDER_NONCE,
    }
}

fn order_hash(order: &OrderV4) -> B256 {
    keccak256(order.abi_encode())
}

/// The claim as the website sends it: takeOrders4 for exactly the coupon's
/// amount, the taker's input being the order's output.
fn take_one(order: &ClaimOrder, probe: SignedCoupon) -> TakeOrdersConfigV5 {
    let amount = probe.context[1];
    TakeOrdersConfigV5 {
        minimumIO: amount,
        maximumIO: amount,
        maximumIORatio: CLAIM_IO_RATIO,
        IOIsInput: true,
        orders: vec![TakeOrderConfigV4 {
            order: claim_order(order),
            inputIOIndex: alloy::primitives::U256::ZERO,
            outputIOIndex: alloy::primitives::U256::ZERO,
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
    use crate::signer::safe::tests::{
        fake_service, own_hash, owner_service, service_owner, signature_json, typed_data_signature,
    };
    use crate::signer::{
        amount_word, context_digest, safe_message_hash, OwnerService, SafeOwners, SIGN_TIMEOUT,
    };
    use alloy::primitives::{address, PrimitiveSignature, U256};
    use alloy::signers::local::PrivateKeySigner;
    use alloy::sol_types::SolCall;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use serde_json::{json, Value};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    const VAULT: Address = address!("E3E064e3C2EEf66cb93dA8D8114F5084E92F48D6");
    const HOT: Address = address!("6c6EE5e31d828De241282B9606C8e98Ea48526E2");
    const ORDERBOOK: Address = address!("37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20");
    const OTHER: Address = address!("1111111111111111111111111111111111111111");
    const KEY: &str = "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";

    fn vault_id() -> B256 {
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
            interpreter: address!("4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf"),
            store: address!("1Aa775533E28B1D843e1A589034984E3a62005DC"),
            bytecode: Bytes::from_static(&[0x00, 0xab]),
            input_token: address!("1234567890123456789012345678901234567890"),
        };
        order.order_hash = order_hash(&claim_order(&order));
        order
    }

    fn signer_for(order: ClaimOrder) -> CouponSigner {
        CouponSigner::for_order(key(), order)
    }

    /// The wei a coupon's amount word carries: its exponent must be -18.
    fn wei_of(word: B256) -> U256 {
        assert_eq!(
            &word.0[..4],
            &[0xff, 0xff, 0xff, 0xee],
            "the amount word's exponent"
        );
        U256::from_be_bytes(word.0) & ((U256::from(1) << 224) - U256::from(1))
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
        vault_id: B256,
        orders: Vec<B256>,
        valid_signer: Address,
        signer_code: &'static str,
        vault_balance: U256,
        /// The vault's admin.
        admin: Address,
        /// What the claim signer answers as a Safe, when it is one.
        safe: Option<FakeSafe>,
        /// The signer claimed the startup probe once, so its nonce is used.
        probe_claimed: bool,
    }

    /// A Safe as the claim signer: its threshold, owners, and the domain
    /// separator it answers, a Safe 1.3+'s on the chain unless given.
    #[derive(Clone)]
    struct FakeSafe {
        threshold: u64,
        owners: Vec<Address>,
        domain: Option<B256>,
        /// Takes one owner's signature, whatever its threshold: a broken Safe.
        takes_one: bool,
        /// What takeOrders4 reverts with when the Safe refuses the coupon's
        /// signatures, in place of InvalidSignature(0): Raindex's checks
        /// reordered, or another revert first.
        refusal: Option<Vec<u8>>,
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
                admin: OTHER,
                safe: None,
                probe_claimed: false,
            }
        }

        /// This chain with `safe` as the claim signer, at `order().signer`
        /// changed to SAFE by the caller.
        fn with_safe(self, threshold: u64, owners: &[PrivateKeySigner]) -> Self {
            Self {
                valid_signer: SAFE,
                signer_code: "0x6080",
                safe: Some(FakeSafe {
                    threshold,
                    owners: owners.iter().map(|key| key.address()).collect(),
                    domain: None,
                    takes_one: false,
                    refusal: None,
                }),
                ..self
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
                            IHoloLockVault::adminCall::SELECTOR => self.admin.abi_encode(),
                            _ => return empty,
                        }
                    } else if to == self.orderbook
                        && selector == IRaindexV6::orderExistsCall::SELECTOR
                    {
                        let asked = IRaindexV6::orderExistsCall::abi_decode(&input, true)
                            .unwrap()
                            .orderHash;
                        self.orders.contains(&asked).abi_encode()
                    } else if to == self.orderbook
                        && selector == IRaindexV6::takeOrders4Call::SELECTOR
                    {
                        return self.take_orders(&input);
                    } else if let Some(safe) =
                        self.safe.as_ref().filter(|_| to == self.valid_signer)
                    {
                        match selector {
                            ISafe::getThresholdCall::SELECTOR => {
                                U256::from(safe.threshold).abi_encode()
                            }
                            ISafe::getOwnersCall::SELECTOR => safe.owners.abi_encode(),
                            ISafe::domainSeparatorCall::SELECTOR => safe
                                .domain
                                .unwrap_or_else(|| safe_domain_separator(self.chain_id, to))
                                .abi_encode(),
                            ISafe::isValidSignatureCall::SELECTOR => {
                                let asked =
                                    ISafe::isValidSignatureCall::abi_decode(&input, true).unwrap();
                                if !safe.accepts(
                                    self.chain_id,
                                    to,
                                    &asked.dataHash,
                                    &asked.signature,
                                ) {
                                    return reverted(
                                        Revert {
                                            reason: "GS026".to_string(),
                                        }
                                        .abi_encode(),
                                    );
                                }
                                alloy::primitives::FixedBytes::<4>::from(
                                    crate::signer::safe::EIP1271_MAGIC_VALUE,
                                )
                                .abi_encode()
                            }
                            _ => return empty,
                        }
                    } else {
                        return empty;
                    };
                    Ok(format!("0x{}", hex::encode(encoded)))
                }
                other => panic!("the check sent {other}"),
            }
        }

        /// takeOrders4 as RaindexV6 runs the claim order: the coupon's signature
        /// first, then the expression's signer check, its nonce check last, then
        /// the vault's balance.
        fn take_orders(&self, input: &[u8]) -> Answer {
            let config = IRaindexV6::takeOrders4Call::abi_decode(input, true)
                .unwrap()
                .config;
            let take = &config.orders[0];
            let coupon = &take.signedContext[0];
            let digest = context_digest(&coupon.context);
            match self
                .safe
                .as_ref()
                .filter(|_| coupon.signer == self.valid_signer)
            {
                Some(safe) => {
                    if !safe.accepts(self.chain_id, coupon.signer, &digest, &coupon.signature) {
                        return reverted(
                            safe.refusal
                                .clone()
                                .unwrap_or_else(|| InvalidSignature { i: U256::ZERO }.abi_encode()),
                        );
                    }
                }
                None => {
                    let recovered = PrimitiveSignature::try_from(coupon.signature.as_ref())
                        .unwrap()
                        .recover_address_from_prehash(&digest)
                        .unwrap();
                    assert_eq!(recovered, coupon.signer, "the probe's signature");
                }
            }
            assert!(
                self.orders.contains(&order_hash(&take.order)),
                "the probe named an order the orderbook does not hold"
            );
            assert!(
                config.IOIsInput,
                "a claim takes the order's output as its input"
            );
            assert_eq!(
                config.maximumIORatio, CLAIM_IO_RATIO,
                "the claim's maximum ratio"
            );
            if coupon.signer != self.valid_signer {
                return reverted(
                    Revert {
                        reason: "Wrong signer".to_string(),
                    }
                    .abi_encode(),
                );
            }
            if self.probe_claimed && coupon.context[8] == crate::signer::probe_nonce() {
                return reverted(
                    Revert {
                        reason: NONCE_USED.to_string(),
                    }
                    .abi_encode(),
                );
            }
            if self.vault_balance < wei_of(config.minimumIO) {
                return reverted(
                    MinimumIO {
                        minimumIO: config.minimumIO,
                        actualIO: B256::ZERO,
                    }
                    .abi_encode(),
                );
            }
            Ok(format!(
                "0x{}",
                hex::encode((config.minimumIO, B256::ZERO).abi_encode_params())
            ))
        }
    }

    impl FakeSafe {
        /// As Safe 1.4.1's checkNSignatures over the SafeMessage of `digest`:
        /// `threshold` owner signatures of 65 bytes, in ascending owner order,
        /// each an owner's.
        fn accepts(&self, chain_id: u64, safe: Address, digest: &B256, signature: &[u8]) -> bool {
            let hash = safe_message_hash(chain_id, safe, digest);
            let fits = if self.takes_one {
                signature.len() >= 65 && signature.len().is_multiple_of(65)
            } else {
                signature.len() == 65 * self.threshold as usize
            };
            if !fits {
                return false;
            }
            let mut last = Address::ZERO;
            for part in signature.chunks(65) {
                let Ok(recovered) = PrimitiveSignature::try_from(part)
                    .and_then(|s| s.recover_address_from_prehash(&hash))
                else {
                    return false;
                };
                if recovered <= last || !self.owners.contains(&recovered) {
                    return false;
                }
                last = recovered;
            }
            true
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
        check_with(network, chain, &signer_for(order.clone())).await
    }

    async fn check_with(network: Network, chain: Chain, signer: &CouponSigner) -> Result<()> {
        let url =
            serve(move |method, params| chain.answer(method, params).map(Value::String)).await;
        check(network, &url, VAULT, signer).await
    }

    /// Anvil's accounts 1, 2 and 3, the owners of the Safe of threshold 2 at
    /// SAFE in test/TestSafeCouponSignature.t.sol.
    const SAFE: Address = address!("3838Fc4E0cba211c3D6D202539bB2dc2B72C7a22");
    const OWNER_KEYS: [&str; 3] = [
        "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
        "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
        "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6",
    ];

    fn owners() -> Vec<PrivateKeySigner> {
        OWNER_KEYS.iter().map(|key| key.parse().unwrap()).collect()
    }

    /// The claim order with SAFE as its signer, and a chain where SAFE is a Safe
    /// of `threshold` with `owners()`.
    fn safe_setup(network: Network, threshold: u64) -> (Chain, ClaimOrder) {
        let (order, chain) = changed(|order| order.signer = SAFE);
        let chain = Chain {
            chain_id: network.chain_id(),
            ..chain
        }
        .with_safe(threshold, &owners());
        (chain, order)
    }

    fn held(keys: &[usize]) -> Vec<Owner> {
        keys.iter()
            .map(|i| Owner::Key(owners()[*i].clone()))
            .collect()
    }

    /// The startup check on `chain`, with the Safe signer asking `asked` and
    /// reading the chain on `signer_chain`.
    async fn safe_check(
        network: Network,
        chain: Chain,
        order: ClaimOrder,
        asked: Vec<Owner>,
        signer_chain: u64,
    ) -> Result<()> {
        safe_check_allowing(network, chain, order, asked, signer_chain, false).await
    }

    /// As `safe_check`, with UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes when `one_party`.
    async fn safe_check_allowing(
        network: Network,
        chain: Chain,
        order: ClaimOrder,
        asked: Vec<Owner>,
        signer_chain: u64,
        one_party: bool,
    ) -> Result<()> {
        let url =
            serve(move |method, params| chain.answer(method, params).map(Value::String)).await;
        let owners =
            SafeOwners::new(SAFE, signer_chain, asked, url.clone())?.allowing_one_party(one_party);
        check(
            network,
            &url,
            VAULT,
            &CouponSigner::for_order(owners, order),
        )
        .await
    }

    async fn safe_refusal(network: Network, chain: Chain, asked: Vec<Owner>) -> String {
        let order = changed(|order| order.signer = SAFE).0;
        format!(
            "{:#}",
            safe_check(network, chain, order, asked, network.chain_id())
                .await
                .expect_err("the check passed")
        )
    }

    /// The Safe's isValidSignature checks the owners' signatures over the
    /// SafeMessage hash, as the probe has it do through takeOrders4, with M
    /// signatures, and refuses M-1. Mainnet needs the override for M keys here.
    #[tokio::test]
    async fn passes_a_safe_claim_signer_whose_threshold_of_owner_keys_it_holds() {
        for (network, one_party) in [(Network::BaseSepolia, false), (Network::Mainnet, true)] {
            let (chain, order) = safe_setup(network, 2);
            safe_check_allowing(
                network,
                chain,
                order,
                held(&[2, 0]),
                network.chain_id(),
                one_party,
            )
            .await
            .unwrap();
        }
    }

    /// The Safe's threshold of owner keys in one process is one party signing as
    /// the Safe, and the Safe is the vault admin too at launch: mainnet refuses
    /// it unless UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes.
    #[tokio::test]
    async fn mainnet_refuses_the_safes_threshold_of_owner_keys_without_the_override() {
        let (chain, _) = safe_setup(Network::Mainnet, 2);

        assert_eq!(
            safe_refusal(Network::Mainnet, chain, held(&[2, 0])).await,
            format!("this orchestrator holds the keys of 2 owners of CLAIM_SIGNER {SAFE}, a Safe of threshold 2, and signs as all of them, which is one party, not 2. Mainnet takes owners at independent signer services, SAFE_OWNER_SERVICES (docs/signer-service.md). To start regardless, set UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes")
        );
    }

    /// When the Safe is also the vault admin, an owner key held here approves
    /// admin transactions as well as coupons: mainnet refuses even one, unless
    /// UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes. A Safe that is not the admin takes it.
    #[tokio::test]
    async fn mainnet_refuses_an_owner_key_of_a_safe_that_is_also_the_vault_admin() {
        let (chain, order) = safe_setup(Network::Mainnet, 2);
        let service = owner_service(owners()[0].clone()).await;
        let asked = || {
            vec![
                service_owner(&owners()[0], &service),
                Owner::Key(owners()[2].clone()),
            ]
        };
        let admin = Chain {
            admin: SAFE,
            ..chain.clone()
        };

        assert_eq!(
            safe_refusal(Network::Mainnet, admin.clone(), asked()).await,
            format!("CLAIM_SIGNER {SAFE} is also the vault's admin, so the 1 owner keys in SAFE_OWNER_KEYS also approve its admin transactions, adminWithdraw among them. Mainnet takes owners at independent signer services, SAFE_OWNER_SERVICES (docs/signer-service.md). To start regardless, set UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes")
        );
        safe_check_allowing(Network::Mainnet, admin, order.clone(), asked(), 1, true)
            .await
            .unwrap();
        safe_check(Network::Mainnet, chain, order, asked(), 1)
            .await
            .unwrap();
    }

    /// A Safe of threshold 1 is one owner signing as the Safe, and two sets of
    /// one owner share none: every network refuses it, with
    /// UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes too.
    #[tokio::test]
    async fn refuses_a_safe_of_threshold_1_even_with_the_override() {
        let services = [
            owner_service(owners()[0].clone()).await,
            owner_service(owners()[1].clone()).await,
        ];
        let asked = || {
            vec![
                service_owner(&owners()[0], &services[0]),
                service_owner(&owners()[1], &services[1]),
            ]
        };
        let (chain, order) = safe_setup(Network::Mainnet, 1);

        assert_eq!(
            safe_refusal(Network::Mainnet, chain.clone(), asked()).await,
            format!("CLAIM_SIGNER {SAFE} is a Safe of threshold 1 with 3 owners, so two sets of 1 of its owners can share none: after a claim order rotation, owners that never signed a withdrawal could sign it again for the new order. Its threshold must be more than half its owners (docs/signer-service.md#one-order-per-withdrawal)")
        );
        let err = safe_check_allowing(Network::Mainnet, chain, order, asked(), 1, true)
            .await
            .expect_err("the override took a Safe of threshold 1");
        assert!(format!("{err:#}").starts_with(&format!(
            "CLAIM_SIGNER {SAFE} is a Safe of threshold 1 with 3 owners"
        )));
        for service in &services {
            assert!(service.requests.lock().unwrap().is_empty());
        }

        let (chain, _) = safe_setup(Network::BaseSepolia, 1);
        assert!(safe_refusal(Network::BaseSepolia, chain, held(&[0, 1]))
            .await
            .starts_with(&format!(
                "CLAIM_SIGNER {SAFE} is a Safe of threshold 1 with 3 owners"
            )));
    }

    #[tokio::test]
    async fn refuses_fewer_owners_that_can_sign_than_the_safes_threshold() {
        let (chain, _) = safe_setup(Network::Mainnet, 3);
        let able = [0, 1]
            .map(|i| format!("{}, a key held here", owners()[i].address()))
            .join("; ");

        assert_eq!(
            safe_refusal(Network::Mainnet, chain, held(&[0, 1])).await,
            format!("2 of the owners this orchestrator asks can sign now, and CLAIM_SIGNER {SAFE} is a Safe of threshold 3: {able}")
        );
    }

    #[tokio::test]
    async fn refuses_more_safe_owner_keys_than_the_safes_threshold() {
        let (chain, _) = safe_setup(Network::Mainnet, 2);

        assert_eq!(
            safe_refusal(Network::Mainnet, chain, held(&[0, 1, 2])).await,
            format!("SAFE_OWNER_KEYS holds 3 keys, and CLAIM_SIGNER {SAFE} is a Safe of threshold 2: give the keys of that many of its owners")
        );
    }

    #[tokio::test]
    async fn refuses_a_safe_owner_key_that_is_not_an_owner() {
        let (chain, _) = safe_setup(Network::Mainnet, 2);
        let stranger = PrivateKeySigner::random();
        let chain = chain.with_safe(
            2,
            &[owners()[0].clone(), stranger.clone(), owners()[2].clone()],
        );
        let listed = [
            owners()[0].address(),
            stranger.address(),
            owners()[2].address(),
        ]
        .map(|owner| owner.to_string())
        .join(", ");

        assert_eq!(
            safe_refusal(Network::Mainnet, chain, held(&[0, 1])).await,
            format!(
                "SAFE_OWNER_KEYS holds the key of {}, which is not an owner of CLAIM_SIGNER {SAFE}: its owners are {listed}",
                owners()[1].address()
            )
        );
    }

    #[tokio::test]
    async fn refuses_a_safe_with_another_eip712_domain() {
        let (mut chain, _) = safe_setup(Network::Mainnet, 2);
        chain.safe.as_mut().unwrap().domain = Some(safe_domain_separator(84_532, SAFE));

        assert_eq!(
            safe_refusal(Network::Mainnet, chain, held(&[0, 1])).await,
            format!("CLAIM_SIGNER {SAFE} does not sign under the EIP-712 domain of a Safe 1.3.0 or later on chain 1, so its owners' signatures would not be over the hash it checks")
        );
    }

    #[tokio::test]
    async fn refuses_a_contract_claim_signer_that_is_not_a_safe_for_safe_owner_keys() {
        let (mut chain, _) = safe_setup(Network::Mainnet, 2);
        chain.safe = None;

        assert_eq!(
            safe_refusal(Network::Mainnet, chain, held(&[0, 1])).await,
            format!("CLAIM_SIGNER {SAFE} is a contract but not a Safe: it did not answer getThreshold()")
        );
    }

    #[tokio::test]
    async fn refuses_safe_owner_keys_for_a_claim_signer_that_is_a_key() {
        let (mut chain, _) = safe_setup(Network::Mainnet, 2);
        chain.safe = None;
        chain.signer_code = "0x";

        assert_eq!(
            safe_refusal(Network::Mainnet, chain, held(&[0, 1])).await,
            format!("CLAIM_SIGNER {SAFE} has no contract, so it is a key, not a Safe, and SAFE_OWNER_KEYS and SAFE_OWNER_SERVICES sign for a Safe. For a key, set SIGNER_PRIVATE_KEY instead")
        );
    }

    /// The owners' signatures are checked by the Safe before the probe goes to
    /// the orderbook. Here the keys sign under another chain's domain, which no
    /// setting can cause, as the Safe's chain and the signer's are both
    /// NETWORK's.
    #[tokio::test]
    async fn a_safe_refusing_its_owners_signatures_is_reported() {
        let (chain, order) = safe_setup(Network::Mainnet, 2);

        let err = safe_check_allowing(Network::Mainnet, chain, order, held(&[0, 1]), 84_532, true)
            .await
            .unwrap_err();

        assert_eq!(
            format!("{err:#}"),
            format!("CLAIM_SIGNER {SAFE} refused the signatures of 2 of its owners: its isValidSignature did not accept them")
        );
    }

    /// A Safe that takes fewer signatures than its threshold says is refused:
    /// the probe signed by M-1 owners must fail.
    #[tokio::test]
    async fn refuses_a_safe_that_takes_fewer_than_its_threshold() {
        let (mut chain, order) = safe_setup(Network::Mainnet, 2);
        chain.safe.as_mut().unwrap().takes_one = true;

        let err = safe_check_allowing(Network::Mainnet, chain, order, held(&[0, 1]), 1, true)
            .await
            .unwrap_err();

        assert_eq!(
            format!("{err:#}"),
            format!("the claim order ORDER_HASH took a coupon carrying 1 owner signatures for CLAIM_SIGNER {SAFE}, fewer than its threshold")
        );
    }

    /// The probe signed by M-1 owners must fail with InvalidSignature(0), the
    /// Safe refusing its signatures: another revert does not show that.
    #[tokio::test]
    async fn refuses_a_safe_whose_m_minus_1_probe_fails_with_another_revert() {
        let refusal = |revert: Vec<u8>| async move {
            let (mut chain, order) = safe_setup(Network::BaseSepolia, 2);
            chain.safe.as_mut().unwrap().refusal = Some(revert);
            format!(
                "{:#}",
                safe_check(Network::BaseSepolia, chain, order, held(&[0, 1]), 84_532)
                    .await
                    .unwrap_err()
            )
        };
        let refused = |why: &str| {
            format!("the claim order ORDER_HASH refused a coupon carrying 1 owner signatures for CLAIM_SIGNER {SAFE} with {why}, not InvalidSignature(0): that does not show the Safe refuses fewer than its threshold")
        };

        assert_eq!(
            refusal(
                Revert {
                    reason: "Order expired".to_string()
                }
                .abi_encode()
            )
            .await,
            refused("\"Order expired\"")
        );
        assert_eq!(
            refusal(InvalidSignature { i: U256::from(1) }.abi_encode()).await,
            refused("error 0x52bf9848")
        );
    }

    /// Owners at signer services: each answers GET /signer for its owner, the
    /// Safe and the chain, and signs the probe, which pays no withdrawal. One
    /// that is down is left out while the others are at least the threshold.
    #[tokio::test]
    async fn passes_a_safe_whose_owners_sign_at_their_signer_services_with_one_down() {
        let (chain, order) = safe_setup(Network::Mainnet, 2);
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let down = format!("http://{}/", closed.local_addr().unwrap());
        drop(closed);
        let (a, b) = (
            owner_service(owners()[0].clone()).await,
            owner_service(owners()[1].clone()).await,
        );
        let asked = vec![
            service_owner(&owners()[0], &a),
            service_owner(&owners()[1], &b),
            Owner::Service(
                OwnerService::new(Network::Mainnet, owners()[2].address(), &down).unwrap(),
            ),
        ];

        safe_check(Network::Mainnet, chain, order, asked, 1)
            .await
            .unwrap();

        let paths: Vec<String> = a
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _)| path.clone())
            .collect();
        assert_eq!(paths, ["/holo/signer", "/holo/sign"]);
        assert_eq!(a.requests.lock().unwrap()[1].1["withdrawal"], Value::Null);
    }

    /// A signer service that takes each connection and never answers, and how
    /// many requests it was sent.
    async fn black_hole() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    read_request(&mut socket).await;
                    seen.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<()>().await;
                    drop(socket);
                });
            }
        });
        (url, requests)
    }

    /// A signer service for `key` that answers GET /signer, and takes POST
    /// /sign without ever answering it.
    async fn never_signs(key: PrivateKeySigner) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let address = key.address();
                tokio::spawn(async move {
                    let mut head = [0u8; 64];
                    let peeked = socket.peek(&mut head).await.unwrap();
                    let identity = head[..peeked].starts_with(b"GET /signer ");
                    read_request(&mut socket).await;
                    if !identity {
                        std::future::pending::<()>().await;
                    }
                    let reply = json!({"owner": address, "safe": SAFE, "chainId": 1}).to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        url
    }

    /// Lal's regression on PR #2: a service that takes the connection and never
    /// answers costs the startup check one wait for GET /signer, and is not
    /// asked to sign the probe, so the two others start a Safe of 2 of 3 well
    /// within RPC_TIMEOUT. A closed port, as above, refuses at once and shows
    /// none of this.
    #[tokio::test]
    async fn a_black_holed_signer_service_leaves_two_of_three_to_start() {
        let (chain, order) = safe_setup(Network::Mainnet, 2);
        let (down, requests) = black_hole().await;
        let (a, b) = (
            owner_service(owners()[0].clone()).await,
            owner_service(owners()[1].clone()).await,
        );
        let asked = vec![
            service_owner(&owners()[0], &a),
            service_owner(&owners()[1], &b),
            Owner::Service(
                OwnerService::new(Network::Mainnet, owners()[2].address(), &down).unwrap(),
            ),
        ];

        let started = std::time::Instant::now();
        let result = safe_check(Network::Mainnet, chain, order, asked, 1).await;

        assert!(
            result.is_ok(),
            "two answering owners meet the Safe's threshold of 2, after {:?}: {result:?}",
            started.elapsed()
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "the black-holed service is asked GET /signer, and not to sign the probe"
        );
        for service in [&a, &b] {
            assert_eq!(service.requests.lock().unwrap().len(), 2);
        }
    }

    /// The probe needs M signatures, and nothing more: a service that answers
    /// GET /signer but never signs does not hold the startup check up for its
    /// SIGN_TIMEOUT once the two others have signed.
    #[tokio::test]
    async fn the_startup_probe_ends_once_m_owners_have_signed() {
        let (chain, order) = safe_setup(Network::Mainnet, 2);
        let silent = never_signs(owners()[2].clone()).await;
        let (a, b) = (
            owner_service(owners()[0].clone()).await,
            owner_service(owners()[1].clone()).await,
        );
        let asked = vec![
            service_owner(&owners()[0], &a),
            service_owner(&owners()[1], &b),
            Owner::Service(
                OwnerService::new(Network::Mainnet, owners()[2].address(), &silent).unwrap(),
            ),
        ];

        let started = std::time::Instant::now();
        safe_check(Network::Mainnet, chain, order, asked, 1)
            .await
            .unwrap();

        assert!(
            started.elapsed() < SIGN_TIMEOUT,
            "the probe waited {:?} for an owner it did not need",
            started.elapsed()
        );
    }

    /// A signer service for `key` that answers GET /signer at once, and
    /// POST /sign after `delay`.
    async fn signs_after(key: PrivateKeySigner, delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let key = key.clone();
                tokio::spawn(async move {
                    let mut head = [0u8; 64];
                    let peeked = socket.peek(&mut head).await.unwrap();
                    let identity = head[..peeked].starts_with(b"GET /signer ");
                    let body = read_request(&mut socket).await;
                    let reply = if identity {
                        json!({"owner": key.address(), "safe": SAFE, "chainId": 1}).to_string()
                    } else {
                        tokio::time::sleep(delay).await;
                        let body: Value = serde_json::from_slice(&body).unwrap();
                        signature_json(key.address(), &typed_data_signature(&own_hash(&body), &key))
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        url
    }

    /// A signer service's time to sign the probe is its own, SIGN_TIMEOUT, and
    /// not the RPC's: the RPC deadline covers the chain reads, not the owners'
    /// signatures. A needed service that signs after longer than the RPC
    /// deadline starts the Safe, and is not reported as the RPC not answering.
    #[tokio::test]
    async fn a_signer_service_slow_to_sign_the_probe_is_not_the_rpc_not_answering() {
        let (chain, order) = safe_setup(Network::Mainnet, 2);
        let slow = signs_after(owners()[0].clone(), Duration::from_millis(600)).await;
        let quick = owner_service(owners()[1].clone()).await;
        let asked = vec![
            Owner::Service(
                OwnerService::new(Network::Mainnet, owners()[0].address(), &slow).unwrap(),
            ),
            service_owner(&owners()[1], &quick),
        ];
        let url =
            serve(move |method, params| chain.answer(method, params).map(Value::String)).await;
        let signer =
            CouponSigner::for_order(SafeOwners::new(SAFE, 1, asked, url.clone()).unwrap(), order);

        let result = check_within(
            Duration::from_millis(300),
            Network::Mainnet,
            &url,
            VAULT,
            &signer,
        )
        .await;

        assert!(
            result.is_ok(),
            "a service signing the probe in 600ms, within SIGN_TIMEOUT, failed the check under an RPC deadline of 300ms: {result:?}"
        );
    }

    /// Anvil's account 4: a fourth owner.
    const FOURTH_OWNER_KEY: &str =
        "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a";

    /// A Safe of 2 of 4 has two sets of 2 owners that share none, so after a
    /// claim order rotation two owners that never signed a withdrawal could
    /// sign it again for the new order (Lal's review of PR #2). Every network
    /// refuses it, before any signer service is asked, with
    /// UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes too: the override is about who holds
    /// the keys, not about paying one withdrawal twice.
    #[tokio::test]
    async fn refuses_a_safe_whose_threshold_is_half_its_owners_or_fewer() {
        let four = [owners(), vec![FOURTH_OWNER_KEY.parse().unwrap()]].concat();
        let refused = format!("CLAIM_SIGNER {SAFE} is a Safe of threshold 2 with 4 owners, so two sets of 2 of its owners can share none: after a claim order rotation, owners that never signed a withdrawal could sign it again for the new order. Its threshold must be more than half its owners (docs/signer-service.md#one-order-per-withdrawal)");

        let (chain, order) = safe_setup(Network::Mainnet, 2);
        let chain = chain.with_safe(2, &four);
        let services = [
            owner_service(owners()[0].clone()).await,
            owner_service(owners()[1].clone()).await,
        ];
        let asked = || {
            vec![
                service_owner(&owners()[0], &services[0]),
                service_owner(&owners()[1], &services[1]),
            ]
        };
        let err = safe_check(Network::Mainnet, chain.clone(), order.clone(), asked(), 1)
            .await
            .expect_err("mainnet took a Safe of 2 of 4");
        assert_eq!(format!("{err:#}"), refused);
        for service in &services {
            assert!(service.requests.lock().unwrap().is_empty());
        }
        let err = safe_check_allowing(Network::Mainnet, chain, order, asked(), 1, true)
            .await
            .expect_err("the override took a Safe of 2 of 4 on mainnet");
        assert_eq!(format!("{err:#}"), refused);
        for service in &services {
            assert!(service.requests.lock().unwrap().is_empty());
        }

        let (chain, order) = safe_setup(Network::BaseSepolia, 2);
        let chain = chain.with_safe(2, &four);
        let err = safe_check(
            Network::BaseSepolia,
            chain.clone(),
            order.clone(),
            held(&[0, 1]),
            84_532,
        )
        .await
        .expect_err("base-sepolia took a Safe of 2 of 4");
        assert_eq!(format!("{err:#}"), refused);
        let err = safe_check_allowing(
            Network::BaseSepolia,
            chain,
            order,
            held(&[0, 1]),
            84_532,
            true,
        )
        .await
        .expect_err("the override took a Safe of 2 of 4 on base-sepolia");
        assert_eq!(format!("{err:#}"), refused);
    }

    #[tokio::test]
    async fn refuses_a_signer_service_answering_for_another_owner_safe_or_chain() {
        let (chain, _) = safe_setup(Network::Mainnet, 2);
        let other_owner = owner_service(owners()[1].clone()).await;
        let asked = vec![
            service_owner(&owners()[0], &other_owner),
            Owner::Key(owners()[2].clone()),
        ];
        let host = asked[0].name();
        let host = host
            .split("signer service at ")
            .nth(1)
            .unwrap()
            .trim_end_matches(')')
            .to_string();

        assert_eq!(
            safe_refusal(Network::Mainnet, chain.clone(), asked).await,
            format!(
                "the signer service at {host} signs as {} for {SAFE} on chain 1, and SAFE_OWNER_SERVICES names it for {}, an owner of CLAIM_SIGNER {SAFE} on chain 1",
                owners()[1].address(),
                owners()[0].address()
            )
        );

        let other_chain = fake_service(move |_, _| {
            (
                "200 OK",
                json!({"owner": owners()[0].address(), "safe": SAFE, "chainId": 84532}).to_string(),
            )
        })
        .await;
        let asked = vec![
            service_owner(&owners()[0], &other_chain),
            Owner::Key(owners()[2].clone()),
        ];
        assert!(safe_refusal(Network::Mainnet, chain, asked)
            .await
            .contains("on chain 84532, and SAFE_OWNER_SERVICES names it"));
    }

    #[tokio::test]
    async fn refuses_a_signer_service_for_an_address_that_is_not_an_owner() {
        let (chain, _) = safe_setup(Network::Mainnet, 2);
        let stranger = PrivateKeySigner::random();
        let service = owner_service(stranger.clone()).await;
        let asked = vec![
            service_owner(&stranger, &service),
            Owner::Key(owners()[2].clone()),
        ];

        assert!(safe_refusal(Network::Mainnet, chain, asked)
            .await
            .starts_with(&format!(
                "SAFE_OWNER_SERVICES names {}, which is not an owner of CLAIM_SIGNER {SAFE}",
                stranger.address()
            )));
    }

    /// `order` changed by `change`, with its hash rebuilt, and a chain that holds it.
    fn changed(change: impl FnOnce(&mut ClaimOrder)) -> (ClaimOrder, Chain) {
        let mut order = order();
        change(&mut order);
        order.order_hash = order_hash(&claim_order(&order));
        let chain = Chain {
            orders: vec![order.order_hash],
            ..Chain::of(Network::BaseSepolia)
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
        for network in [Network::BaseSepolia, Network::Mainnet] {
            check_against(network, Chain::of(network), &order())
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn refuses_an_rpc_that_answers_for_the_other_network() {
        assert_eq!(
            refusal(Network::Mainnet, Chain::of(Network::BaseSepolia), &order()).await,
            "ETH_RPC_URL answers for chain 84532, and NETWORK=mainnet is chain 1"
        );
        assert_eq!(
            refusal(Network::BaseSepolia, Chain::of(Network::Mainnet), &order()).await,
            "BASE_SEPOLIA_RPC_URL answers for chain 1, and NETWORK=base-sepolia is chain 84532"
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
        let one = B256::with_last_byte(1);
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
                    vault_id: one,
                    ..Chain::of(Network::Mainnet)
                },
                format!("VAULT_ID {} is not the vault's vaultId {one}", vault_id()),
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
            refusal(Network::BaseSepolia, chain, &order).await,
            format!("ORDER_OWNER {OTHER} is not the vault BASE_SEPOLIA_LOCK_VAULT_ADDRESS {VAULT}")
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
                Network::BaseSepolia,
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
            "BASE_SEPOLIA_RPC_URL did not answer within 200ms"
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
            "ORDER_HASH is not the hash of the order that ORDER_OWNER, TOKEN_ADDRESS, VAULT_ID, CLAIM_INTERPRETER, CLAIM_STORE, CLAIM_BYTECODE and CLAIM_INPUT_TOKEN describe"
        );
    }

    /// The order hash test vector: test/TestCouponSignature.t.sol hashes the same
    /// OrderV4 with Solidity's abi.encode, and ui/src/lib/network.test.ts with viem.
    #[test]
    fn hashes_the_claim_order_as_raindex_does() {
        let vector = ClaimOrder::vector();

        assert_eq!(
            order_hash(&claim_order(&vector)),
            "0xaa24ef105ecbfc39695b240de6baeb09278fd4ebf75674b29bcbd2c7ef72ff0b"
                .parse::<B256>()
                .unwrap()
        );
    }

    #[tokio::test]
    async fn refuses_a_key_the_claim_order_does_not_accept() {
        for network in [Network::BaseSepolia, Network::Mainnet] {
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

    /// The signer once claimed the probe, which only it can: its fixed nonce is
    /// used, and the claim order answers "Nonce already used", its last check.
    /// The startup passes, for a key and for a Safe, whose M-1 probe is still
    /// refused for its signatures first.
    #[tokio::test]
    async fn passes_a_claim_order_whose_probe_its_signer_claimed_once() {
        for network in [Network::BaseSepolia, Network::Mainnet] {
            let chain = Chain {
                probe_claimed: true,
                ..Chain::of(network)
            };
            check_against(network, chain, &order()).await.unwrap();
        }
        let (mut chain, order) = safe_setup(Network::BaseSepolia, 2);
        chain.probe_claimed = true;
        safe_check(Network::BaseSepolia, chain, order, held(&[0, 1]), 84_532)
            .await
            .unwrap();
    }

    /// The startup takes "Nonce already used" as the claim order accepting its
    /// signer only because the nonce is the expression's last check, after the
    /// signer and every other word of the coupon.
    #[test]
    fn the_nonce_is_the_claim_orders_last_check() {
        let rain = include_str!("../../src/holo-claim.rain");
        let calculate = rain
            .split_once("\n#calculate-io\n")
            .and_then(|(_, rest)| rest.split_once("\n#handle-io\n"))
            .map(|(calculate, _)| calculate)
            .expect("the claim expression's #calculate-io");
        let ensures: Vec<&str> = calculate
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with(":ensure("))
            .collect();

        assert_eq!(ensures.len(), 9, "{ensures:#?}");
        assert!(ensures[0].ends_with("\"Wrong signer\"),"), "{ensures:#?}");
        assert!(
            ensures[8].ends_with(&format!("\"{NONCE_USED}\"),")),
            "{ensures:#?}"
        );
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
        for network in [Network::BaseSepolia, Network::Mainnet] {
            let chain = Chain {
                signer_code: "0x608060405260043610",
                ..Chain::of(network)
            };

            assert_eq!(
                refusal(network, chain, &order()).await,
                format!(
                    "CLAIM_SIGNER {} is a contract, such as a Safe, and SIGNER_PRIVATE_KEY signs as a key, so it cannot sign for it. For a Safe, set SAFE_OWNER_KEYS or SAFE_OWNER_SERVICES to its owners instead",
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
            refusal(Network::BaseSepolia, chain, &order).await,
            format!(
                "CLAIM_SIGNER {OTHER} is a contract, such as a Safe, and SIGNER_PRIVATE_KEY signs as a key, so it cannot sign for it. For a Safe, set SAFE_OWNER_KEYS or SAFE_OWNER_SERVICES to its owners instead"
            )
        );
    }

    #[tokio::test]
    async fn refuses_a_key_that_is_not_the_claim_signer() {
        let (order, chain) = changed(|order| order.signer = OTHER);

        assert_eq!(
            refusal(Network::BaseSepolia, chain, &order).await,
            format!(
                "SIGNER_PRIVATE_KEY is the key of {}, not of CLAIM_SIGNER {OTHER}",
                key().address()
            )
        );
    }

    /// Anvil's public accounts carry such a delegation on mainnet, which is how
    /// this was found: Raindex V6 refused their coupons with InvalidSignature.
    #[tokio::test]
    async fn refuses_a_claim_signer_that_is_a_key_with_an_eip_7702_delegation() {
        let chain = Chain {
            signer_code: "0xef01001111111111111111111111111111111111111111",
            ..Chain::of(Network::Mainnet)
        };

        assert_eq!(
            refusal(Network::Mainnet, chain, &order()).await,
            format!(
                "CLAIM_SIGNER {} has an EIP-7702 delegation: Raindex V6 asks the delegate contract for a coupon's signature in place of the key, so the key's coupons are refused. Use a key with no delegation",
                key().address()
            )
        );
    }

    #[test]
    fn the_probe_claims_one_wei_at_the_claim_orders_ratio() {
        let probe = SignedCoupon {
            signer: key().address(),
            context: vec![B256::ZERO, amount_word(U256::from(1)).unwrap()],
            signature: vec![],
        };

        let config = take_one(&order(), probe);

        assert_eq!(wei_of(config.minimumIO), U256::from(1));
        assert_eq!(config.maximumIO, config.minimumIO);
        assert_eq!(config.maximumIORatio, CLAIM_IO_RATIO);
        assert!(config.IOIsInput);
        assert_eq!(
            CLAIM_IO_RATIO,
            "0xffffffe200000000000000000000000000000000000000000000000000000001"
                .parse::<B256>()
                .unwrap()
        );
    }

    /// The claim calldata test vector: test/TestCouponSignature.t.sol encodes the
    /// same takeOrders4 call with abi.encodeCall, and ui/src/lib/claim.test.ts sends it.
    #[test]
    fn encodes_the_claim_as_forge_does() {
        let signer = address!("3C44CdDdB6a900fa2b585dd299e03d12FA4293BC");
        let order = ClaimOrder {
            order_hash: "0xaa24ef105ecbfc39695b240de6baeb09278fd4ebf75674b29bcbd2c7ef72ff0b"
                .parse()
                .unwrap(),
            signer,
            ..ClaimOrder::vector()
        };
        let words: Vec<B256> = [
            "0x0000000000000000000000001111111111111111111111111111111111111111",
            "0xffffffee000000000000000000000000000000000000000014d1120d7b160000",
            "0x0000000000000000000000000000000000000000000000000000000068581c00",
            "0xaa24ef105ecbfc39695b240de6baeb09278fd4ebf75674b29bcbd2c7ef72ff0b",
            "0x000000000000000000000000e3e064e3c2eef66cb93da8d8114f5084e92f48d6",
            "0x00000000000000000000000037fc0efec37d19f8a221aa4f8f7600c9ba2acd20",
            "0x0000000000000000000000006c6ee5e31d828de241282b9606c8e98ea48526e2",
            "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b",
            "0x12c911abe545a1669fcbcdaeeede5352ee5a6743f59dbacdc0251b1e4c2f7b77",
        ]
        .iter()
        .map(|word| word.parse().unwrap())
        .collect();
        let signature = hex::decode("f3a686a6e1eccbfba52fbcde8a66bd2eca7bfd5b88bb326cb5486f6c4596e5bf7abe61d43b8e825201362259d527971715ee2d362460c97006ca29cb1355515a1b").unwrap();
        let coupon = SignedCoupon {
            signer,
            context: words,
            signature,
        };

        let calldata = IRaindexV6::takeOrders4Call {
            config: take_one(&order, coupon),
        }
        .abi_encode();

        assert_eq!(
            keccak256(&calldata),
            "0x9c90a13c518aa17629ca92bb5947f54e0fcfb1c0ef65376c17d8ea02e7763522"
                .parse::<B256>()
                .unwrap()
        );
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
                accepts_signer(OTHER, outcome(taken, "ETH_RPC_URL").unwrap()).unwrap_err()
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
