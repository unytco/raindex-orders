// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Script, console2} from "forge-std/Script.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {
    MAINNET_ORDERBOOK,
    MAINNET_HOT,
    SEPOLIA_ORDERBOOK,
    SEPOLIA_TROT,
    HOLO_VAULT_ID,
    MIN_LOCK_AMOUNT
} from "src/Constants.sol";

/// @title DeployHoloLockVault
/// @notice Deploys the HoloLockVault contract to mainnet or Sepolia
/// @dev Run with:
///   # Sepolia (dry-run):
///   forge script script/DeployHoloLockVault.s.sol:DeploySepoliaHoloLockVault --rpc-url $SEPOLIA_RPC_URL
///
///   # Sepolia (broadcast):
///   forge script script/DeployHoloLockVault.s.sol:DeploySepoliaHoloLockVault --rpc-url $SEPOLIA_RPC_URL --broadcast --verify
///
///   # Mainnet (dry-run):
///   forge script script/DeployHoloLockVault.s.sol:DeployMainnetHoloLockVault --rpc-url $ETH_RPC_URL
contract DeploySepoliaHoloLockVault is Script {
    function run() external {
        // Read admin address from environment or use deployer
        address admin = vm.envOr("ADMIN_ADDRESS", msg.sender);

        console2.log("Deploying HoloLockVault to Sepolia...");
        console2.log("Token (TROT):", address(SEPOLIA_TROT));
        console2.log("Orderbook:", address(SEPOLIA_ORDERBOOK));
        console2.log("Vault ID:", HOLO_VAULT_ID);
        console2.log("Admin:", admin);
        console2.log("Min Lock Amount:", MIN_LOCK_AMOUNT);

        vm.startBroadcast();

        HoloLockVault lockVault = new HoloLockVault(
            address(SEPOLIA_TROT),
            address(SEPOLIA_ORDERBOOK),
            HOLO_VAULT_ID,
            admin,
            MIN_LOCK_AMOUNT
        );

        vm.stopBroadcast();

        console2.log("HoloLockVault deployed at:", address(lockVault));
    }
}

contract DeployMainnetHoloLockVault is Script {
    function run() external {
        // Read admin address from environment
        address admin = vm.envAddress("ADMIN_ADDRESS");

        console2.log("Deploying HoloLockVault to Mainnet...");
        console2.log("Token (HOT):", address(MAINNET_HOT));
        console2.log("Orderbook:", address(MAINNET_ORDERBOOK));
        console2.log("Vault ID:", HOLO_VAULT_ID);
        console2.log("Admin:", admin);
        console2.log("Min Lock Amount:", MIN_LOCK_AMOUNT);

        vm.startBroadcast();

        HoloLockVault lockVault = new HoloLockVault(
            address(MAINNET_HOT),
            address(MAINNET_ORDERBOOK),
            HOLO_VAULT_ID,
            admin,
            MIN_LOCK_AMOUNT
        );

        vm.stopBroadcast();

        console2.log("HoloLockVault deployed at:", address(lockVault));
    }
}

/// @notice Deploy with a custom token address (useful for testing with your own test token)
contract DeployHoloLockVaultCustomToken is Script {
    function run() external {
        address token = vm.envAddress("TOKEN_ADDRESS");
        address orderbook = vm.envAddress("ORDERBOOK_ADDRESS");
        uint256 vaultId = vm.envOr("VAULT_ID", HOLO_VAULT_ID);
        address admin = vm.envOr("ADMIN_ADDRESS", msg.sender);
        uint256 minLockAmount = vm.envOr("MIN_LOCK_AMOUNT", MIN_LOCK_AMOUNT);

        console2.log("Deploying HoloLockVault with custom token...");
        console2.log("Token:", token);
        console2.log("Orderbook:", orderbook);
        console2.log("Vault ID:", vaultId);
        console2.log("Admin:", admin);
        console2.log("Min Lock Amount:", minLockAmount);

        vm.startBroadcast();

        HoloLockVault lockVault = new HoloLockVault(
            token,
            orderbook,
            vaultId,
            admin,
            minLockAmount
        );

        vm.stopBroadcast();

        console2.log("HoloLockVault deployed at:", address(lockVault));
    }
}
