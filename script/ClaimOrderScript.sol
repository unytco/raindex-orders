// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {Script, console2} from "forge-std/Script.sol";
import {Vm} from "forge-std/Vm.sol";
import {
    IRaindexV6,
    OrderConfigV4,
    OrderV4,
    IOV2,
    EvaluableV4
} from "raindex-interface-0.1.7/src/interface/IRaindexV6.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {ClaimInputToken} from "src/ClaimInputToken.sol";
import {
    ICompatibilityFallbackHandler,
    ISafe,
    LibSafeSetup,
    SAFE_SINGLETON,
    SAFE_L2_SINGLETON,
    SAFE_FALLBACK_HANDLER,
    SAFE_SINGLETON_CODEHASH,
    SAFE_L2_SINGLETON_CODEHASH,
    SAFE_FALLBACK_HANDLER_CODEHASH,
    SAFE_PROXY_CODEHASH,
    SAFE_SINGLETON_SLOT,
    SAFE_FALLBACK_HANDLER_SLOT,
    SAFE_GUARD_SLOT,
    SAFE_SENTINEL_MODULES
} from "./Safe.sol";
import {
    RAINDEX,
    RAINDEX_CODEHASH,
    RAINDEX_SUBPARSER,
    RAINDEX_SUBPARSER_CODEHASH,
    RAINLANG_DEPLOYER,
    RAINLANG_DEPLOYER_HASH,
    RAINLANG_INTERPRETER,
    RAINLANG_INTERPRETER_HASH,
    RAINLANG_STORE,
    RAINLANG_STORE_HASH,
    TEST_SIGNER_ADDRESS,
    HOLO_VAULT_ID,
    CLAIM_ORDER_NONCE
} from "src/Constants.sol";

/// What a Safe answers about itself.
interface ISafeView {
    function getThreshold() external view returns (uint256);
    function getOwners() external view returns (address[] memory);
}

// The override that lets a mainnet deploy hand the vault to one key.
string constant SINGLE_KEY_ADMIN_OVERRIDE = "UNSAFE_ALLOW_SINGLE_KEY_ADMIN";

/// What the operator states the vault admin is: whether one key may hold it,
/// and the threshold and owners of the Safe that does.
struct AdminInputs {
    bool singleKeyAllowed;
    uint256 threshold;
    address[] owners;
}

/// One network's claim order: its name, chain, and the variable its lock vault
/// is passed as. Raindex V6 and the Rainlang contracts are at the same address on
/// every chain, so the network names nothing else. The TestNet is Base Sepolia.
struct ClaimNetwork {
    string name;
    uint256 chainId;
    string vaultVar;
}

abstract contract ClaimOrderScript is Script {
    bytes32 private constant ADD_ORDER_V3 = keccak256(
        "AddOrderV3(address,bytes32,(address,(address,address,bytes),(address,bytes32)[],(address,bytes32)[],bytes32))"
    );

    function claimNetwork(string memory name) internal pure returns (ClaimNetwork memory) {
        bytes32 key = keccak256(bytes(name));
        if (key == keccak256("mainnet")) {
            return ClaimNetwork("mainnet", 1, "MAINNET_LOCK_VAULT_ADDRESS");
        }
        if (key == keccak256("base-sepolia")) {
            return ClaimNetwork("base-sepolia", 84532, "BASE_SEPOLIA_LOCK_VAULT_ADDRESS");
        }
        revert(string.concat("NETWORK must be base-sepolia or mainnet, not ", name));
    }

    /// NETWORK's claim order, base-sepolia when NETWORK is unset.
    function networkFromEnv() internal view returns (ClaimNetwork memory net) {
        net = claimNetwork(vm.envOr("NETWORK", string("base-sepolia")));
        requireChain(net);
    }

    function requireChain(ClaimNetwork memory net) internal view {
        require(
            block.chainid == net.chainId,
            string.concat(
                "the RPC answers for chain ",
                vm.toString(block.chainid),
                ", and NETWORK=",
                net.name,
                " is chain ",
                vm.toString(net.chainId)
            )
        );
    }

    /// The Raindex V6 and Rainlang release src/Constants.sol names must be on the
    /// chain, byte for byte. Rain deploys each through its deterministic deployer
    /// to the same address on every chain, on Ethereum mainnet and Base Sepolia
    /// among them.
    function requireRainV6() internal view {
        requireRelease("RaindexV6", address(RAINDEX), RAINDEX_CODEHASH);
        requireRelease("RaindexV6SubParser", RAINDEX_SUBPARSER, RAINDEX_SUBPARSER_CODEHASH);
        requireRelease("RainlangExpressionDeployer", address(RAINLANG_DEPLOYER), RAINLANG_DEPLOYER_HASH);
        requireRelease("RainlangInterpreter", address(RAINLANG_INTERPRETER), RAINLANG_INTERPRETER_HASH);
        requireRelease("RainlangStore", address(RAINLANG_STORE), RAINLANG_STORE_HASH);
    }

    function requireRelease(string memory name, address at, bytes32 codehash) private view {
        require(
            at.code.length > 0,
            string.concat(
                name,
                " is not deployed at ",
                vm.toString(at),
                " on chain ",
                vm.toString(block.chainid),
                ": Rain has not deployed this release there (DEPLOY.md)"
            )
        );
        require(
            at.codehash == codehash,
            string.concat(name, " at ", vm.toString(at), " is not the release src/Constants.sol names")
        );
    }

    /// A key with an EIP-7702 delegation: its code is the 23-byte designator.
    function isDelegatedKey(address account) internal view returns (bool) {
        bytes memory code = account.code;
        return code.length == 23 && code[0] == 0xef && code[1] == 0x01 && code[2] == 0x00;
    }

    /// An EIP-7702 delegation leaves a key a key, for sending transactions.
    function isContract(address account) internal view returns (bool) {
        return account.code.length > 0 && !isDelegatedKey(account);
    }

    /// Refuses the test signer on mainnet, a key with an EIP-7702 delegation, whose
    /// coupons Raindex V6 refuses (it asks any account with code through EIP-1271,
    /// never by the key's signature), and a contract signer whose signatures the
    /// website cannot read: it reads a Safe's 2 to 20 owner signatures, and 65 bytes
    /// as a key's. A Safe's threshold must be more than half its owners, so that
    /// every two sets of signers share an owner: each signer service refuses a
    /// withdrawal it signed for another claim order, and only then is one of
    /// them in every set that could sign it for this one
    /// (docs/signer-service.md#one-order-per-withdrawal). A Safe must be one
    /// only its owners act as (`safeFault`).
    function requireSigner(ClaimNetwork memory net, address signer) internal view {
        require(signer != address(0), "VALID_SIGNER must be a nonzero address");
        require(
            net.chainId != 1 || signer != TEST_SIGNER_ADDRESS,
            "VALID_SIGNER is the test signer, whose key is public: mainnet refuses it"
        );
        require(
            !isDelegatedKey(signer),
            "VALID_SIGNER has an EIP-7702 delegation, which Raindex V6 asks for a coupon's signature in place of the key: use a key with no delegation, or a Safe"
        );
        if (isContract(signer)) {
            (bool isSafe, uint256 threshold, address[] memory owners) = safeOf(signer);
            require(
                isSafe && threshold >= 2 && threshold <= 20,
                "VALID_SIGNER is a contract but not a Safe whose threshold is 2 to 20"
            );
            require(
                threshold * 2 > owners.length,
                string.concat(
                    "VALID_SIGNER ",
                    vm.toString(signer),
                    " is a Safe of threshold ",
                    vm.toString(threshold),
                    " with ",
                    vm.toString(owners.length),
                    " owners, so two sets of ",
                    vm.toString(threshold),
                    " of its owners can share none: after a claim order rotation, owners that never signed a withdrawal could sign it again for the new order. Its threshold must be more than half its owners (docs/signer-service.md#one-order-per-withdrawal)"
                )
            );
            require(
                hashesSafeMessages(signer),
                "VALID_SIGNER is a Safe whose fallback handler does not hash coupons as Safe's CompatibilityFallbackHandler does, so it would refuse every coupon: set that handler on it (docs/enable-multisig.md)"
            );
            requireOnlyOwnersAct("VALID_SIGNER", signer);
        }
    }

    /// Refuses `safe`, the account `name` names, unless only its threshold of
    /// owners can act as it (`safeFault`).
    function requireOnlyOwnersAct(string memory name, address safe) internal view {
        string memory fault = safeFault(safe);
        require(
            bytes(fault).length == 0,
            string.concat(
                name,
                " ",
                vm.toString(safe),
                " is not a Safe 1.4.1 that only its owners act as: ",
                fault,
                " (docs/enable-multisig.md)"
            )
        );
    }

    /// Why something other than `safe`'s threshold of owners could act as it, or
    /// "" when nothing can. It must be Safe's SafeProxy 1.4.1 on the Safe 1.4.1 or
    /// SafeL2 1.4.1 singleton, with the CompatibilityFallbackHandler 1.4.1, each
    /// byte for byte, and have no guard and no module. A module executes the
    /// Safe's transactions with no owner's signature: as the vault admin it could
    /// take the vault's HOT, and as the coupon signer mark any coupon signed. A
    /// guard can refuse any transaction the owners sign, and another handler
    /// answers the coupons' EIP-1271 question as it likes.
    function safeFault(address safe) internal view returns (string memory) {
        if (safe.codehash != SAFE_PROXY_CODEHASH) {
            return "its code is not Safe's SafeProxy 1.4.1";
        }
        address singleton = address(uint160(uint256(vm.load(safe, SAFE_SINGLETON_SLOT))));
        if (
            !(singleton == SAFE_SINGLETON && singleton.codehash == SAFE_SINGLETON_CODEHASH)
                && !(singleton == SAFE_L2_SINGLETON && singleton.codehash == SAFE_L2_SINGLETON_CODEHASH)
        ) {
            return string.concat("it runs the singleton ", vm.toString(singleton), ", not Safe 1.4.1 or SafeL2 1.4.1");
        }
        address handler = address(uint160(uint256(vm.load(safe, SAFE_FALLBACK_HANDLER_SLOT))));
        if (handler != address(SAFE_FALLBACK_HANDLER) || handler.codehash != SAFE_FALLBACK_HANDLER_CODEHASH) {
            return string.concat(
                "its fallback handler is ", vm.toString(handler), ", not the CompatibilityFallbackHandler 1.4.1"
            );
        }
        address guard = address(uint160(uint256(vm.load(safe, SAFE_GUARD_SLOT))));
        if (guard != address(0)) {
            return string.concat("it has the guard ", vm.toString(guard), ", which can refuse what its owners sign");
        }
        (address[] memory modules,) = ISafe(safe).getModulesPaginated(SAFE_SENTINEL_MODULES, 1);
        if (modules.length != 0) {
            return string.concat(
                "it has the module ",
                vm.toString(modules[0]),
                " enabled, which acts as the Safe with no owner's signature"
            );
        }
        return "";
    }

    /// Whether the Safe `safe` answers EIP-1271 for coupons: its fallback
    /// handler, asked through the Safe, hashes a digest into the SafeMessage hash
    /// its owners sign, on this chain. A Safe with no handler, or another, fails.
    function hashesSafeMessages(address safe) internal view returns (bool) {
        bytes32 digest = keccak256("a coupon digest");
        (bool answered, bytes memory answer) =
            safe.staticcall(abi.encodeCall(ICompatibilityFallbackHandler.getMessageHash, (abi.encode(digest))));
        return answered && answer.length == 32
            && abi.decode(answer, (bytes32)) == LibSafeSetup.messageHash(block.chainid, safe, digest);
    }

    /// Logs whether one Safe holds both roles: the vault admin and the coupon
    /// signer. That is the launch shape (docs/enable-multisig.md): the owners who
    /// can sign a coupon could take the vault's HOT either way, and a coupon's
    /// owner signatures cannot pass as an admin transaction's.
    function logRoles(address admin, address signer) internal pure {
        if (admin == signer) {
            console2.log(
                string.concat(
                    "VALID_SIGNER is ADMIN_ADDRESS ",
                    vm.toString(admin),
                    ": one Safe is the vault admin and the coupon signer"
                )
            );
        }
    }

    /// Whether `account` answers as a Safe, and its threshold and owners if so.
    function safeOf(address account) internal view returns (bool isSafe, uint256 threshold, address[] memory owners) {
        (bool answered, bytes memory answer) = account.staticcall(abi.encodeCall(ISafeView.getThreshold, ()));
        if (!answered || answer.length != 32) return (false, 0, owners);
        threshold = abi.decode(answer, (uint256));
        (answered, answer) = account.staticcall(abi.encodeCall(ISafeView.getOwners, ()));
        if (!answered || answer.length < 64) return (false, 0, owners);
        owners = abi.decode(answer, (address[]));
        isSafe = threshold >= 1 && owners.length >= threshold;
    }

    /// The vault admin a mainnet deploy hands the bridge to, from its first
    /// transaction: a Safe whose threshold is 2 or more, so that no one key
    /// controls the vault. `given` is what the operator takes the Safe to be, its
    /// threshold and owners, and must match what it answers, so the vault cannot
    /// go to a Safe other than the one meant. One key, or a Safe of threshold 1,
    /// is refused unless the override is set, which is logged. A Safe must also
    /// be one only its owners act as, with no module or guard (`safeFault`).
    function requireAdmin(address admin, AdminInputs memory given) internal view {
        require(admin != address(0), "ADMIN_ADDRESS must be a nonzero address");
        string memory at = vm.toString(admin);
        if (!isContract(admin)) {
            require(
                given.singleKeyAllowed,
                string.concat(
                    "ADMIN_ADDRESS ",
                    at,
                    " is a key, not a Safe: on mainnet the vault admin is a Safe from the first transaction (docs/enable-multisig.md). To hand the vault to one key regardless, set ",
                    SINGLE_KEY_ADMIN_OVERRIDE,
                    "=yes"
                )
            );
            console2.log(
                string.concat(
                    SINGLE_KEY_ADMIN_OVERRIDE, "=yes: ADMIN_ADDRESS ", at, " is one key, which alone controls the vault"
                )
            );
            return;
        }
        (bool isSafe, uint256 threshold, address[] memory owners) = safeOf(admin);
        require(isSafe, string.concat("ADMIN_ADDRESS ", at, " is a contract but not a Safe"));
        string memory shape = string.concat(
            "a Safe of threshold ", vm.toString(threshold), " with ", vm.toString(owners.length), " owners"
        );
        require(
            threshold >= 2 || given.singleKeyAllowed,
            string.concat(
                "ADMIN_ADDRESS ",
                at,
                " is ",
                shape,
                ", which one key controls. Use a Safe of threshold 2 or more, or set ",
                SINGLE_KEY_ADMIN_OVERRIDE,
                "=yes"
            )
        );
        require(
            given.threshold != 0, "ADMIN_SAFE_THRESHOLD is required: the threshold ADMIN_ADDRESS is expected to have"
        );
        require(
            given.threshold == threshold,
            string.concat(
                "ADMIN_ADDRESS ",
                at,
                " is ",
                shape,
                ", not ADMIN_SAFE_THRESHOLD=",
                vm.toString(given.threshold),
                ": is it the Safe meant?"
            )
        );
        require(
            given.owners.length != 0,
            "ADMIN_SAFE_OWNERS is required: the owners ADMIN_ADDRESS is expected to have, separated by commas"
        );
        require(
            sameAddresses(given.owners, owners),
            string.concat(
                "ADMIN_SAFE_OWNERS does not list the owners of ADMIN_ADDRESS ",
                at,
                ", which are ",
                joined(owners),
                ": is it the Safe meant?"
            )
        );
        requireOnlyOwnersAct("ADMIN_ADDRESS", admin);
        console2.log(string.concat("ADMIN_ADDRESS ", at, " is ", shape, ": ", joined(owners)));
        if (threshold < 2) {
            console2.log(string.concat(SINGLE_KEY_ADMIN_OVERRIDE, "=yes: one of those keys alone controls the vault"));
        }
    }

    /// The admin inputs as deploy-mainnet.sh passes them: the override, and
    /// the Safe's expected threshold and owners.
    function adminInputsFromEnv() internal view returns (AdminInputs memory) {
        return AdminInputs(
            keccak256(bytes(vm.envOr(SINGLE_KEY_ADMIN_OVERRIDE, string("")))) == keccak256("yes"),
            vm.envOr("ADMIN_SAFE_THRESHOLD", uint256(0)),
            vm.envOr("ADMIN_SAFE_OWNERS", ",", new address[](0))
        );
    }

    /// Whether `stated` lists exactly the addresses of `actual`, in any order
    /// and naming none twice.
    function sameAddresses(address[] memory stated, address[] memory actual) internal pure returns (bool) {
        if (stated.length != actual.length) return false;
        for (uint256 i = 0; i < stated.length; i++) {
            for (uint256 j = i + 1; j < stated.length; j++) {
                if (stated[i] == stated[j]) return false;
            }
            bool listed;
            for (uint256 j = 0; j < actual.length && !listed; j++) {
                listed = stated[i] == actual[j];
            }
            if (!listed) return false;
        }
        return true;
    }

    function joined(address[] memory addresses) internal pure returns (string memory list) {
        for (uint256 i = 0; i < addresses.length; i++) {
            list = string.concat(list, i == 0 ? "" : ", ", vm.toString(addresses[i]));
        }
    }

    /// The claim order paying `token` out of HOLO_VAULT_ID against `inputToken`,
    /// accepting coupons from `signer`, composed and parsed before anything is
    /// sent.
    function claimOrderConfig(ClaimNetwork memory net, address token, address inputToken, address signer)
        internal
        returns (OrderConfigV4 memory)
    {
        return OrderConfigV4(
            claimEvaluable(net, signer), claimIO(inputToken), claimIO(token), CLAIM_ORDER_NONCE, bytes32(0), ""
        );
    }

    /// The claim order's evaluable: the network's interpreter and store, and the
    /// claim expression accepting coupons from `signer`, parsed.
    function claimEvaluable(ClaimNetwork memory net, address signer) internal returns (EvaluableV4 memory) {
        return EvaluableV4(RAINLANG_INTERPRETER, RAINLANG_STORE, parseClaim(net, signer));
    }

    function claimOrder(address vault, address token, address inputToken, bytes memory bytecode)
        internal
        pure
        returns (OrderV4 memory)
    {
        return OrderV4(
            vault,
            EvaluableV4(RAINLANG_INTERPRETER, RAINLANG_STORE, bytecode),
            claimIO(inputToken),
            claimIO(token),
            CLAIM_ORDER_NONCE
        );
    }

    function orderHashOf(OrderV4 memory order) internal pure returns (bytes32) {
        return keccak256(abi.encode(order));
    }

    function claimIO(address token) private pure returns (IOV2[] memory io) {
        io = new IOV2[](1);
        io[0] = IOV2(token, HOLO_VAULT_ID);
    }

    /// The claim expression for `signer`, composed by compose-rainlang.mjs and
    /// parsed by the network's expression deployer into the order's bytecode.
    function parseClaim(ClaimNetwork memory net, address signer) private returns (bytes memory) {
        string[] memory command = new string[](8);
        command[0] = "node";
        command[1] = "compose-rainlang.mjs";
        command[2] = "--network";
        command[3] = net.name;
        command[4] = "--subparser";
        command[5] = vm.toString(RAINDEX_SUBPARSER);
        command[6] = "--signer";
        command[7] = vm.toString(signer);
        Vm.FfiResult memory composed = vm.tryFfi(command);
        require(composed.exitCode == 0, string.concat("compose-rainlang.mjs failed: ", string(composed.stderr)));
        return RAINLANG_DEPLOYER.parse2(composed.stdout);
    }

    /// Sends `calls` to the vault as its admin, each of which must change the
    /// orderbook. An admin that is a contract, such as a Safe, cannot sign here:
    /// its calls are printed for it to execute.
    function asAdmin(HoloLockVault vault, bytes[] memory calls) internal {
        address admin = vault.admin();
        if (isContract(admin)) {
            console2.log(
                string.concat(
                    "The vault admin ",
                    vm.toString(admin),
                    " is a contract. Nothing was sent. Execute these calls from it, in this order:"
                )
            );
            for (uint256 i = 0; i < calls.length; i++) {
                console2.log(
                    string.concat(
                        vm.toString(i + 1),
                        ". to ",
                        vm.toString(address(vault)),
                        ", value 0, data ",
                        vm.toString(calls[i])
                    )
                );
            }
            return;
        }
        vm.startBroadcast();
        (, address sender,) = vm.readCallers();
        require(sender == admin, string.concat("pass the vault admin's wallet: the admin is ", vm.toString(admin)));
        for (uint256 i = 0; i < calls.length; i++) {
            (bool ok, bytes memory result) = address(vault).call(calls[i]);
            if (!ok) {
                assembly ("memory-safe") {
                    revert(add(result, 0x20), mload(result))
                }
            }
            require(abi.decode(result, (bool)), "the orderbook already held that order, or no longer did");
        }
        vm.stopBroadcast();
    }

    /// The claim order `vault` added last from `fromBlock` on, read from the chain
    /// and checked against it: the only order the vault added since that is still on
    /// the orderbook, paying the vault's token out of HOLO_VAULT_ID against a
    /// ClaimInputToken, run by the network's interpreter and store, and carrying
    /// the claim expression that accepts coupons from `signer` and nothing else.
    function findClaimOrder(ClaimNetwork memory net, HoloLockVault vault, address signer, uint256 fromBlock)
        internal
        returns (OrderV4 memory order, bytes32 orderHash)
    {
        requireSigner(net, signer);
        bytes32[] memory topics = new bytes32[](1);
        topics[0] = ADD_ORDER_V3;
        Vm.EthGetLogs[] memory logs = vm.eth_getLogs(fromBlock, block.number, address(RAINDEX), topics);
        bool found;
        for (uint256 i = 0; i < logs.length; i++) {
            (address sender, bytes32 addedHash, OrderV4 memory added) =
                abi.decode(logs[i].data, (address, bytes32, OrderV4));
            if (sender != address(vault)) continue;
            if (found) {
                require(
                    !RAINDEX.orderExists(orderHash),
                    string.concat("the vault holds another order added since block ", vm.toString(fromBlock))
                );
            }
            (order, orderHash, found) = (added, addedHash, true);
        }
        require(found, string.concat("the vault added no order from block ", vm.toString(fromBlock)));
        require(
            order.evaluable.interpreter == RAINLANG_INTERPRETER && order.evaluable.store == RAINLANG_STORE,
            "the claim order is not run by the network's interpreter and store"
        );
        require(orderHashOf(order) == orderHash, "the AddOrderV3 event's order does not hash to its order hash");
        require(RAINDEX.orderExists(orderHash), "the vault's last claim order is no longer on the orderbook");
        require(
            order.validInputs.length == 1 && order.validOutputs.length == 1
                && orderHashOf(
                    claimOrder(
                        address(vault), address(vault.token()), order.validInputs[0].token, order.evaluable.bytecode
                    )
                ) == orderHash,
            "the vault's last order is not a claim order paying its token out of HOLO_VAULT_ID"
        );
        require(
            order.validInputs[0].token.codehash == keccak256(type(ClaimInputToken).runtimeCode),
            "the claim order's input token is not a ClaimInputToken"
        );
        require(
            keccak256(order.evaluable.bytecode) == keccak256(parseClaim(net, signer)),
            "the claim order's expression is not the claim expression for VALID_SIGNER"
        );
    }

    /// The values the orchestrator and the website take, under their names.
    function printRecord(
        ClaimNetwork memory net,
        HoloLockVault vault,
        address signer,
        OrderV4 memory order,
        bytes32 orderHash
    ) internal view {
        string memory vaultAddress = vm.toString(address(vault));
        string memory token = vm.toString(order.validOutputs[0].token);
        string memory orderbook = vm.toString(address(vault.orderbook()));
        string memory hash = vm.toString(orderHash);
        string memory bytecode = vm.toString(order.evaluable.bytecode);
        console2.log("Deploy record");
        console2.log("bridge-orchestrator:");
        console2.log(string.concat("NETWORK=", net.name));
        console2.log(string.concat(net.vaultVar, "=", vaultAddress));
        console2.log(string.concat("TOKEN_ADDRESS=", token));
        console2.log(string.concat("ORDERBOOK_ADDRESS=", orderbook));
        console2.log(string.concat("VAULT_ID=", vm.toString(order.validOutputs[0].vaultId)));
        console2.log(string.concat("ORDER_HASH=", hash));
        console2.log(string.concat("ORDER_OWNER=", vm.toString(order.owner)));
        console2.log(string.concat("CLAIM_SIGNER=", vm.toString(signer)));
        console2.log(string.concat("CLAIM_INTERPRETER=", vm.toString(address(order.evaluable.interpreter))));
        console2.log(string.concat("CLAIM_STORE=", vm.toString(address(order.evaluable.store))));
        console2.log(string.concat("CLAIM_BYTECODE=", bytecode));
        console2.log(string.concat("CLAIM_INPUT_TOKEN=", vm.toString(order.validInputs[0].token)));
        console2.log("website:");
        console2.log(string.concat("PUBLIC_NETWORK=", net.name));
        console2.log(string.concat("PUBLIC_TOKEN_ADDRESS=", token));
        console2.log(string.concat("PUBLIC_LOCK_VAULT_ADDRESS=", vaultAddress));
        console2.log(string.concat("PUBLIC_ORDERBOOK_ADDRESS=", orderbook));
        console2.log(string.concat("PUBLIC_CLAIM_ORDER_HASH=", hash));
        console2.log(string.concat("PUBLIC_CLAIM_SIGNER=", vm.toString(signer)));
        console2.log(string.concat("PUBLIC_CLAIM_INTERPRETER=", vm.toString(address(order.evaluable.interpreter))));
        console2.log(string.concat("PUBLIC_CLAIM_STORE=", vm.toString(address(order.evaluable.store))));
        console2.log(string.concat("PUBLIC_CLAIM_BYTECODE=", bytecode));
        console2.log(string.concat("PUBLIC_CLAIM_INPUT_TOKEN=", vm.toString(order.validInputs[0].token)));
    }
}
