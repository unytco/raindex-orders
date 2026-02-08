// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Script, console2} from "forge-std/Script.sol";
import {Vm} from "forge-std/Vm.sol";
import {IOrderBookV3, OrderConfigV2, IO} from "rain.orderbook.interface/interface/IOrderBookV3.sol";
import {IParserV1} from "rain.interpreter.interface/interface/IParserV1.sol";
import {IExpressionDeployerV3} from "rain.interpreter.interface/interface/IExpressionDeployerV3.sol";
import {EvaluableConfigV3} from "rain.interpreter.interface/interface/IInterpreterCallerV2.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {
    SEPOLIA_ORDERBOOK,
    SEPOLIA_DEPLOYER,
    SEPOLIA_NOOP,
    HOLO_VAULT_ID
} from "src/Constants.sol";

interface GetParser {
    function iParser() external view returns (IParserV1);
}

/// @title DeployClaimOrderViaVault
/// @notice Deploys the holo-claim order through HoloLockVault so the vault owns the order
/// @dev This ensures claim outputs come from the same vault where locked tokens are deposited
contract DeployClaimOrderViaVault is Script {
    function run() public {
        // Read from environment
        address token = vm.envAddress("TOKEN_ADDRESS");
        address vaultAddress = vm.envAddress("LOCK_VAULT_ADDRESS");

        // Read rainlang from file (more reliable for multi-line content)
        string memory rainlangFile = vm.envString("RAINLANG_FILE");
        string memory rainlang = vm.readFile(rainlangFile);

        console2.log("Deploying Claim Order via HoloLockVault...");
        console2.log("Token:", token);
        console2.log("Vault:", vaultAddress);
        console2.log("Orderbook:", address(SEPOLIA_ORDERBOOK));
        console2.log("Rainlang file:", rainlangFile);
        console2.log("Rainlang length:", bytes(rainlang).length);

        HoloLockVault vault = HoloLockVault(vaultAddress);

        // Parse the rainlang
        IParserV1 parser = GetParser(address(SEPOLIA_DEPLOYER)).iParser();
        (bytes memory bytecode, uint256[] memory constants) = parser.parse(bytes(rainlang));

        console2.log("Bytecode length:", bytecode.length);
        console2.log("Constants count:", constants.length);

        // Build the order config
        EvaluableConfigV3 memory evaluableConfig = EvaluableConfigV3(
            SEPOLIA_DEPLOYER,
            bytecode,
            constants
        );

        // Input: NOOP token (placeholder - claims don't need input)
        IO[] memory inputs = new IO[](1);
        inputs[0] = IO(address(SEPOLIA_NOOP), 18, HOLO_VAULT_ID);

        // Output: The token we're distributing (MockHOT)
        IO[] memory outputs = new IO[](1);
        outputs[0] = IO(token, 18, HOLO_VAULT_ID);

        OrderConfigV2 memory orderConfig = OrderConfigV2(
            inputs,
            outputs,
            evaluableConfig,
            ""
        );

        // Deploy the order through the vault (vault becomes the order owner)
        vm.startBroadcast();
        bool success = vault.addOrder(orderConfig);
        vm.stopBroadcast();

        require(success, "Order already exists");
        console2.log("Order deployed through vault successfully!");
        console2.log("Order owner:", vaultAddress);
    }
}
