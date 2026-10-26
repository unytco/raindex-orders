// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {console2} from "forge-std/Script.sol";
import {
    ISafe,
    LibSafeSetup,
    SAFE_PROXY_FACTORY,
    SAFE_SINGLETON,
    SAFE_L2_SINGLETON,
    SAFE_FALLBACK_HANDLER,
    SAFE_PROXY_FACTORY_CODEHASH,
    SAFE_SINGLETON_CODEHASH,
    SAFE_L2_SINGLETON_CODEHASH,
    SAFE_FALLBACK_HANDLER_CODEHASH,
    SAFE_SINGLETON_SLOT,
    SAFE_FALLBACK_HANDLER_SLOT
} from "./Safe.sol";
import {ClaimOrderScript, ClaimNetwork} from "./ClaimOrderScript.sol";

/// Creates the bridge's Safe, the vault admin and the coupon signer, through
/// Safe's own SafeProxyFactory 1.4.1: owners SAFE_OWNERS, threshold
/// SAFE_THRESHOLD, the CompatibilityFallbackHandler, which answers the claim
/// order's EIP-1271 question for coupons, and no module or guard. On mainnet the
/// singleton is Safe 1.4.1, elsewhere SafeL2 1.4.1, as Safe's own app creates
/// them. The address follows from those inputs and SAFE_SALT_NONCE, so it is
/// printed before anything is sent, and running again with the same inputs finds
/// the same Safe instead of creating another. deploy-mainnet.sh and
/// deploy-base-sepolia.sh run it as their `safe` step.
contract CreateSafe is ClaimOrderScript {
    /// SAFE_SALT_NONCE when it is not set.
    uint256 internal constant DEFAULT_SALT_NONCE = uint256(keccak256("HOT bridge Safe"));
    uint256 internal constant MAX_OWNERS = 20;

    struct Plan {
        address[] owners;
        uint256 threshold;
        uint256 saltNonce;
        address singleton;
        bytes initializer;
        address safe;
    }

    /// Prints where the Safe of these inputs is, and whether it exists. Sends nothing.
    function predict() external view {
        Plan memory plan = planFromEnv();
        if (plan.safe.code.length > 0) {
            requireFound(plan);
            console2.log(string.concat("The Safe exists: ", vm.toString(plan.safe)));
        } else {
            console2.log(string.concat("The Safe would be created at ", vm.toString(plan.safe)));
        }
        printInputs(plan);
    }

    function run() external {
        Plan memory plan = planFromEnv();
        if (plan.safe.code.length > 0) {
            requireFound(plan);
            console2.log(
                string.concat(
                    "The Safe exists, with these owners and threshold. Nothing was sent: ", vm.toString(plan.safe)
                )
            );
        } else {
            console2.log(string.concat("Creating the Safe at ", vm.toString(plan.safe)));
            vm.startBroadcast();
            address created = SAFE_PROXY_FACTORY.createProxyWithNonce(plan.singleton, plan.initializer, plan.saltNonce);
            vm.stopBroadcast();
            require(created == plan.safe, "SafeProxyFactory created the Safe somewhere other than predicted");
            requireFound(plan);
        }
        printInputs(plan);
    }

    function planFromEnv() internal view returns (Plan memory plan) {
        ClaimNetwork memory net = networkFromEnv();
        requireSafeRelease(net);
        plan.owners = vm.envAddress("SAFE_OWNERS", ",");
        plan.threshold = vm.envUint("SAFE_THRESHOLD");
        plan.saltNonce = vm.envOr("SAFE_SALT_NONCE", DEFAULT_SALT_NONCE);
        requireOwners(plan.owners, plan.threshold);
        plan.singleton = net.chainId == 1 ? SAFE_SINGLETON : SAFE_L2_SINGLETON;
        plan.initializer = LibSafeSetup.setupCall(plan.owners, plan.threshold);
        plan.safe = LibSafeSetup.predict(plan.singleton, plan.initializer, plan.saltNonce);
    }

    /// The factory, singletons and handler must be Safe's 1.4.1 release, byte
    /// for byte, at Safe's addresses.
    function requireSafeRelease(ClaimNetwork memory net) internal view {
        requireCode("SafeProxyFactory 1.4.1", address(SAFE_PROXY_FACTORY), SAFE_PROXY_FACTORY_CODEHASH, net);
        requireCode("Safe 1.4.1", SAFE_SINGLETON, SAFE_SINGLETON_CODEHASH, net);
        requireCode("SafeL2 1.4.1", SAFE_L2_SINGLETON, SAFE_L2_SINGLETON_CODEHASH, net);
        requireCode(
            "CompatibilityFallbackHandler 1.4.1", address(SAFE_FALLBACK_HANDLER), SAFE_FALLBACK_HANDLER_CODEHASH, net
        );
    }

    function requireCode(string memory name, address at, bytes32 codehash, ClaimNetwork memory net) private view {
        require(
            at.code.length > 0 && at.codehash == codehash,
            string.concat(name, " is not at ", vm.toString(at), " on ", net.name, ", byte for byte")
        );
    }

    /// 2 to 20 owners, none twice, none zero, none a contract: each owner signs
    /// as a key, at a signer service or in the orchestrator. The threshold is 2
    /// or more, so that no one key acts as the Safe, and at most the owners. It
    /// is more than half the owners, so that every two sets of signers share an
    /// owner: each signer service refuses a withdrawal it signed for another
    /// claim order, and only then is one of them in every set that could sign
    /// it for a new order (docs/signer-service.md#one-order-per-withdrawal).
    function requireOwners(address[] memory owners, uint256 threshold) internal view {
        require(
            owners.length >= 2 && owners.length <= MAX_OWNERS,
            "SAFE_OWNERS lists the Safe's owners, 2 to 20, separated by commas"
        );
        for (uint256 i = 0; i < owners.length; i++) {
            string memory owner = vm.toString(owners[i]);
            require(owners[i] != address(0), "SAFE_OWNERS lists the zero address");
            require(owners[i] != address(1), "SAFE_OWNERS lists 0x...01, which Safe reserves");
            require(
                !isContract(owners[i]),
                string.concat("SAFE_OWNERS lists ", owner, ", a contract: each owner signs as a key")
            );
            for (uint256 j = 0; j < i; j++) {
                require(owners[i] != owners[j], string.concat("SAFE_OWNERS lists ", owner, " twice"));
            }
        }
        require(
            threshold >= 2 && threshold <= owners.length,
            string.concat(
                "SAFE_THRESHOLD is how many owners must sign, 2 to the ",
                vm.toString(owners.length),
                " SAFE_OWNERS lists, so that no one key acts as the Safe"
            )
        );
        require(
            threshold * 2 > owners.length,
            string.concat(
                "SAFE_THRESHOLD ",
                vm.toString(threshold),
                " is half the ",
                vm.toString(owners.length),
                " SAFE_OWNERS or fewer, so two sets of ",
                vm.toString(threshold),
                " owners could share none and sign one withdrawal for two claim orders: it must be more than half (docs/signer-service.md#one-order-per-withdrawal)"
            )
        );
    }

    /// The Safe at the predicted address must be the one these inputs create,
    /// and still have their owners and threshold: owners changed since would
    /// mean it is not the Safe the operator states. A module or guard added
    /// since is refused too.
    function requireFound(Plan memory plan) internal view {
        ISafe safe = ISafe(plan.safe);
        string memory at = vm.toString(plan.safe);
        require(
            address(uint160(uint256(vm.load(plan.safe, SAFE_SINGLETON_SLOT)))) == plan.singleton,
            string.concat("the Safe at ", at, " runs another singleton")
        );
        require(
            address(uint160(uint256(vm.load(plan.safe, SAFE_FALLBACK_HANDLER_SLOT)))) == address(SAFE_FALLBACK_HANDLER),
            string.concat("the Safe at ", at, " has another fallback handler")
        );
        require(
            safe.getThreshold() == plan.threshold && sameAddresses(plan.owners, safe.getOwners()),
            string.concat(
                "the Safe at ",
                at,
                " was created with SAFE_OWNERS and SAFE_THRESHOLD, and its owners or threshold have changed since: its owners are ",
                joined(safe.getOwners()),
                ", its threshold ",
                vm.toString(safe.getThreshold())
            )
        );
        require(hashesSafeMessages(plan.safe), string.concat("the Safe at ", at, " does not hash coupons as expected"));
        requireOnlyOwnersAct("the Safe at", plan.safe);
    }

    /// What the deploy takes next, under its names: the Safe as the vault admin,
    /// with the threshold and owners it is expected to have, and as the coupon signer.
    function printInputs(Plan memory plan) internal pure {
        string memory safe = vm.toString(plan.safe);
        console2.log(string.concat("SAFE=", safe));
        console2.log(string.concat("SAFE_SALT_NONCE=", vm.toString(plan.saltNonce)));
        console2.log("The deploy takes:");
        console2.log(string.concat("ADMIN_ADDRESS=", safe));
        console2.log(string.concat("ADMIN_SAFE_THRESHOLD=", vm.toString(plan.threshold)));
        console2.log(string.concat("ADMIN_SAFE_OWNERS=", joinedPlain(plan.owners)));
        console2.log(string.concat("VALID_SIGNER=", safe));
    }

    function joinedPlain(address[] memory addresses) internal pure returns (string memory list) {
        for (uint256 i = 0; i < addresses.length; i++) {
            list = string.concat(list, i == 0 ? "" : ",", vm.toString(addresses[i]));
        }
    }
}
