// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Script, console2} from "forge-std/Script.sol";
import {MockHOT} from "src/test/MockHOT.sol";

/// @title DeployTestHOT
/// @notice Deploys a mock HOT token for testing on Sepolia
/// @dev Run with:
///   # Sepolia (dry-run):
///   forge script script/DeployTestHOT.s.sol:DeployTestHOT --rpc-url $SEPOLIA_RPC_URL
///
///   # Sepolia (broadcast):
///   forge script script/DeployTestHOT.s.sol:DeployTestHOT --rpc-url $SEPOLIA_RPC_URL --broadcast --verify
contract DeployTestHOT is Script {
    function run() external {
        console2.log("Deploying Mock HOT Token...");

        vm.startBroadcast();

        MockHOT testHOT = new MockHOT();

        vm.stopBroadcast();

        console2.log("MockHOT deployed at:", address(testHOT));
        console2.log("Name:", testHOT.name());
        console2.log("Symbol:", testHOT.symbol());
        console2.log("Decimals:", testHOT.decimals());
    }
}

/// @notice Mint test tokens to an address
/// @dev Run with:
///   TOKEN_ADDRESS=0x... RECIPIENT=0x... AMOUNT=1000000000000000000000 \
///   forge script script/DeployTestHOT.s.sol:MintTestHOT --rpc-url $SEPOLIA_RPC_URL --broadcast
contract MintTestHOT is Script {
    function run() external {
        address tokenAddress = vm.envAddress("TOKEN_ADDRESS");
        address recipient = vm.envAddress("RECIPIENT");
        uint256 amount = vm.envUint("AMOUNT");

        console2.log("Minting", amount, "tokens to", recipient);

        vm.startBroadcast();

        MockHOT(tokenAddress).mint(recipient, amount);

        vm.stopBroadcast();

        console2.log("Minted successfully!");
        console2.log("New balance:", MockHOT(tokenAddress).balanceOf(recipient));
    }
}
