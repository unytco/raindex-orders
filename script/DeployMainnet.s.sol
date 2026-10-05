// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {OrderConfigV2, OrderV2} from "rain.orderbook.interface/interface/IOrderBookV3.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {MAINNET_HOT, MAINNET_ORDERBOOK, HOLO_VAULT_ID, MIN_LOCK_AMOUNT} from "src/Constants.sol";
import {ClaimOrderScript, ClaimNetwork} from "./ClaimOrderScript.sol";

/// @title DeployMainnet
/// @notice Deploys the HOT bridge on Ethereum mainnet against the existing
/// OrderBookV3: HoloLockVault, then the claim order through it, then hands the
/// vault to ADMIN_ADDRESS. Run it through deploy-mainnet.sh, which also reads
/// back what landed and prints the deploy record.
contract DeployMainnet is ClaimOrderScript {
    function run() external {
        ClaimNetwork memory net = claimNetwork("mainnet");
        requireChain(net);
        address admin = vm.envAddress("ADMIN_ADDRESS");
        address signer = vm.envAddress("VALID_SIGNER");
        require(admin != address(0), "ADMIN_ADDRESS must be a nonzero address");
        requireSigner(net, signer);

        OrderConfigV2 memory claim = claimOrderConfig(net, address(MAINNET_HOT), signer);

        vm.startBroadcast();
        (, address deployer,) = vm.readCallers();
        require(deployer != DEFAULT_SENDER, "pass the deployer as a forge wallet option: --ledger or --account");
        require(admin != deployer, "ADMIN_ADDRESS is the deployer, which would stay admin after the handover");
        HoloLockVault vault = new HoloLockVault(
            address(MAINNET_HOT), address(MAINNET_ORDERBOOK), HOLO_VAULT_ID, deployer, MIN_LOCK_AMOUNT
        );
        require(vault.addOrder(claim), "the orderbook already holds this claim order");
        vault.setAdmin(admin);
        vm.stopBroadcast();
    }

    /// Reads back, from the chain, the vault deployed at `fromBlock` and its claim
    /// order, and prints the deploy record.
    function record(HoloLockVault vault, uint256 fromBlock) external {
        ClaimNetwork memory net = claimNetwork("mainnet");
        requireChain(net);
        require(address(vault.token()) == address(MAINNET_HOT), "the vault's token is not HOT");
        require(address(vault.orderbook()) == address(MAINNET_ORDERBOOK), "the vault's orderbook is not OrderBookV3");
        require(vault.vaultId() == HOLO_VAULT_ID, "the vault's vault ID is not HOLO_VAULT_ID");
        require(vault.admin() == vm.envAddress("ADMIN_ADDRESS"), "the vault's admin is not ADMIN_ADDRESS");
        (OrderV2 memory order, bytes32 orderHash) = findClaimOrder(net, vault, fromBlock);
        printRecord(net, vault, vm.envAddress("VALID_SIGNER"), order, orderHash);
    }
}
