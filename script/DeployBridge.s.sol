// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {console2} from "forge-std/Script.sol";
import {Vm} from "forge-std/Vm.sol";
import {OrderV4, EvaluableV4} from "raindex-interface-0.1.7/src/interface/IRaindexV6.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {HoloBridgeDeployer} from "src/HoloBridgeDeployer.sol";
import {RAINDEX, HOLO_VAULT_ID, MIN_LOCK_AMOUNT, CLAIM_ORDER_NONCE} from "src/Constants.sol";
import {ClaimOrderScript, ClaimNetwork} from "./ClaimOrderScript.sol";

/// Deploys the HOT bridge against the RaindexV6 Rain deployed on the network,
/// in one transaction: a HoloBridgeDeployer, which deploys the claim input token
/// and HoloLockVault, adds the claim order through the vault, and hands the vault
/// to ADMIN_ADDRESS, a Safe. No key is the vault's admin at any point. The claim
/// order takes coupons from VALID_SIGNER, which may be that same Safe.
/// deploy-mainnet.sh and deploy-base-sepolia.sh run it, then `record`.
abstract contract DeployBridge is ClaimOrderScript {
    bytes32 private constant ADMIN_CHANGED = keccak256("AdminChanged(address,address)");

    function deploy(ClaimNetwork memory net, address token) internal {
        requireChain(net);
        requireRainV6();
        require(token.code.length > 0, string.concat("the token ", vm.toString(token), " has no contract"));
        address admin = vm.envAddress("ADMIN_ADDRESS");
        address signer = vm.envAddress("VALID_SIGNER");
        requireAdmin(admin, adminInputsFromEnv());
        requireSigner(net, signer);
        logRoles(admin, signer);
        EvaluableV4 memory evaluable = claimEvaluable(net, signer);

        vm.startBroadcast();
        (, address deployer,) = vm.readCallers();
        require(deployer != DEFAULT_SENDER, "pass the deployer as a forge wallet option: --ledger or --account");
        require(admin != deployer, "ADMIN_ADDRESS is the deployer, which would then hold the vault");
        HoloBridgeDeployer bridge = new HoloBridgeDeployer(
            token, address(RAINDEX), HOLO_VAULT_ID, MIN_LOCK_AMOUNT, evaluable, CLAIM_ORDER_NONCE, admin
        );
        vm.stopBroadcast();
        console2.log(string.concat("HoloBridgeDeployer=", vm.toString(address(bridge))));
        console2.log(string.concat("HoloLockVault=", vm.toString(address(bridge.vault()))));
        console2.log(string.concat("ClaimInputToken=", vm.toString(address(bridge.inputToken()))));
    }

    /// Prints the deploy record of the vault `bridge` deployed, read from the
    /// chain, and of the claim order it added from `fromBlock` on. Refuses a
    /// vault whose admin was ever anything but `bridge` and then ADMIN_ADDRESS.
    function recordOf(ClaimNetwork memory net, address token, HoloBridgeDeployer bridge, uint256 fromBlock) internal {
        requireChain(net);
        (bool answered, bytes memory answer) = address(bridge).staticcall(abi.encodeCall(bridge.vault, ()));
        require(answered && answer.length == 32, "pass the HoloBridgeDeployer the deploy printed, not the vault");
        HoloLockVault vault = HoloLockVault(abi.decode(answer, (address)));
        require(address(vault.token()) == token, "the vault's token is not the network's HOT");
        require(address(vault.orderbook()) == address(RAINDEX), "the vault's orderbook is not RaindexV6");
        require(vault.vaultId() == HOLO_VAULT_ID, "the vault's vault ID is not HOLO_VAULT_ID");
        address admin = vm.envAddress("ADMIN_ADDRESS");
        require(vault.admin() == admin, "the vault's admin is not ADMIN_ADDRESS");
        requireHandedOverOnce(vault, address(bridge), admin, fromBlock);
        address signer = vm.envAddress("VALID_SIGNER");
        (OrderV4 memory order, bytes32 orderHash) = findClaimOrder(net, vault, signer, fromBlock);
        require(
            order.validInputs[0].token == address(bridge.inputToken()),
            "the claim order's input token is not the one the HoloBridgeDeployer deployed"
        );
        logRoles(admin, signer);
        console2.log(string.concat("HoloBridgeDeployer=", vm.toString(address(bridge))));
        printRecord(net, vault, signer, order, orderHash);
    }

    /// The vault's AdminChanged events from `fromBlock` on must be the one
    /// handover, from `bridge` to `admin`: no key held the vault in between.
    function requireHandedOverOnce(HoloLockVault vault, address bridge, address admin, uint256 fromBlock) private {
        bytes32[] memory topics = new bytes32[](1);
        topics[0] = ADMIN_CHANGED;
        Vm.EthGetLogs[] memory logs = vm.eth_getLogs(fromBlock, block.number, address(vault), topics);
        require(
            logs.length == 1 && logs[0].topics.length == 3,
            string.concat(
                "the vault's admin changed ",
                vm.toString(logs.length),
                " times since block ",
                vm.toString(fromBlock),
                ", not once"
            )
        );
        require(
            address(uint160(uint256(logs[0].topics[1]))) == bridge
                && address(uint160(uint256(logs[0].topics[2]))) == admin,
            "the vault's one admin change is not the HoloBridgeDeployer handing it to ADMIN_ADDRESS"
        );
    }
}

/// Deploys the TestNet bridge on Base Sepolia as the mainnet bridge deploys:
/// one transaction, with ADMIN_ADDRESS, a Safe, as the vault admin from the
/// first block. TOKEN_ADDRESS is the mock HOT deploy-base-sepolia.sh deployed.
contract DeployBaseSepoliaBridge is DeployBridge {
    function run() external {
        deploy(claimNetwork("base-sepolia"), vm.envAddress("TOKEN_ADDRESS"));
    }

    function record(HoloBridgeDeployer bridge, uint256 fromBlock) external {
        recordOf(claimNetwork("base-sepolia"), vm.envAddress("TOKEN_ADDRESS"), bridge, fromBlock);
    }
}
