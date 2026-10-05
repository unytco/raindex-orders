// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Script, console2} from "forge-std/Script.sol";
import {Vm} from "forge-std/Vm.sol";
import {IOrderBookV3, OrderConfigV2, OrderV2, IO} from "rain.orderbook.interface/interface/IOrderBookV3.sol";
import {IParserV1} from "rain.interpreter.interface/interface/IParserV1.sol";
import {IExpressionDeployerV3} from "rain.interpreter.interface/interface/IExpressionDeployerV3.sol";
import {EvaluableConfigV3, EvaluableV2} from "rain.interpreter.interface/interface/IInterpreterCallerV2.sol";
import {LibOrder} from "rain.orderbook/src/lib/LibOrder.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {
    MAINNET_ORDERBOOK,
    MAINNET_DEPLOYER,
    MAINNET_SUBPARSER,
    MAINNET_USDT,
    SEPOLIA_ORDERBOOK,
    SEPOLIA_DEPLOYER,
    SEPOLIA_SUBPARSER,
    SEPOLIA_NOOP,
    TEST_SIGNER_ADDRESS,
    HOLO_VAULT_ID
} from "src/Constants.sol";

interface ExpressionDeployerParts {
    function iParser() external view returns (IParserV1);
    function iInterpreter() external view returns (address);
    function iStore() external view returns (address);
}

interface SafeThreshold {
    function getThreshold() external view returns (uint256);
}

/// One network's claim order: where it is added, what parses it, and the input
/// token it names. The input is a placeholder, as the order's io-ratio is 0.
struct ClaimNetwork {
    string name;
    uint256 chainId;
    IOrderBookV3 orderbook;
    IExpressionDeployerV3 deployer;
    address subparser;
    address inputToken;
    string vaultVar;
}

abstract contract ClaimOrderScript is Script {
    using LibOrder for OrderV2;

    /// Both IOs declare 18 decimals, USDT's 6 included: the website builds the
    /// order with 18, and the order's hash covers the decimals.
    uint8 internal constant CLAIM_IO_DECIMALS = 18;

    bytes32 private constant ADD_ORDER = keccak256(
        "AddOrder(address,address,(address,bool,(address,address,address),(address,uint8,uint256)[],(address,uint8,uint256)[]),bytes32)"
    );

    function claimNetwork(string memory name) internal pure returns (ClaimNetwork memory) {
        bytes32 key = keccak256(bytes(name));
        if (key == keccak256("mainnet")) {
            return ClaimNetwork(
                "mainnet",
                1,
                MAINNET_ORDERBOOK,
                MAINNET_DEPLOYER,
                MAINNET_SUBPARSER,
                address(MAINNET_USDT),
                "MAINNET_LOCK_VAULT_ADDRESS"
            );
        }
        if (key == keccak256("sepolia")) {
            return ClaimNetwork(
                "sepolia",
                11155111,
                SEPOLIA_ORDERBOOK,
                SEPOLIA_DEPLOYER,
                SEPOLIA_SUBPARSER,
                address(SEPOLIA_NOOP),
                "SEPOLIA_LOCK_VAULT_ADDRESS"
            );
        }
        revert(string.concat("NETWORK must be sepolia or mainnet, not ", name));
    }

    function networkFromEnv() internal view returns (ClaimNetwork memory net) {
        net = claimNetwork(vm.envString("NETWORK"));
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

    /// Refuses the test signer on mainnet, and a contract signer whose signatures the
    /// website cannot read: it reads a Safe's 2 to 20 owner signatures, and 65 bytes
    /// as a key's. An EIP-7702 delegation leaves a key a key.
    function requireSigner(ClaimNetwork memory net, address signer) internal view {
        require(signer != address(0), "VALID_SIGNER must be a nonzero address");
        require(
            net.chainId != 1 || signer != TEST_SIGNER_ADDRESS,
            "VALID_SIGNER is the test signer, whose key is public: mainnet refuses it"
        );
        bytes memory code = signer.code;
        bool delegatedKey = code.length == 23 && code[0] == 0xef && code[1] == 0x01 && code[2] == 0x00;
        if (code.length > 0 && !delegatedKey) {
            (bool answered, bytes memory threshold) = signer.staticcall(abi.encodeCall(SafeThreshold.getThreshold, ()));
            require(
                answered && threshold.length == 32 && abi.decode(threshold, (uint256)) >= 2
                    && abi.decode(threshold, (uint256)) <= 20,
                "VALID_SIGNER is a contract but not a Safe whose threshold is 2 to 20"
            );
        }
    }

    /// The claim order paying `token` out of HOLO_VAULT_ID, accepting coupons
    /// from `signer`, composed and parsed before anything is sent.
    function claimOrderConfig(ClaimNetwork memory net, address token, address signer)
        internal
        returns (OrderConfigV2 memory)
    {
        (bytes memory bytecode, uint256[] memory constants) = parseClaim(net, signer);
        return OrderConfigV2(
            claimIO(net.inputToken), claimIO(token), EvaluableConfigV3(net.deployer, bytecode, constants), ""
        );
    }

    function claimOrder(ClaimNetwork memory net, address vault, address token, EvaluableV2 memory evaluable)
        internal
        pure
        returns (OrderV2 memory)
    {
        return OrderV2(vault, true, evaluable, claimIO(net.inputToken), claimIO(token));
    }

    function claimIO(address token) private pure returns (IO[] memory io) {
        io = new IO[](1);
        io[0] = IO(token, CLAIM_IO_DECIMALS, HOLO_VAULT_ID);
    }

    function parseClaim(ClaimNetwork memory net, address signer)
        private
        returns (bytes memory bytecode, uint256[] memory constants)
    {
        string[] memory command = new string[](8);
        command[0] = "node";
        command[1] = "compose-rainlang.mjs";
        command[2] = "--network";
        command[3] = net.name;
        command[4] = "--subparser";
        command[5] = vm.toString(net.subparser);
        command[6] = "--signer";
        command[7] = vm.toString(signer);
        Vm.FfiResult memory composed = vm.tryFfi(command);
        require(composed.exitCode == 0, string.concat("compose-rainlang.mjs failed: ", string(composed.stderr)));
        return ExpressionDeployerParts(address(net.deployer)).iParser().parse(composed.stdout);
    }

    /// Sends `calls` to the vault as its admin, each of which must change the
    /// orderbook. An admin that is a contract, such as a Safe, cannot sign here:
    /// its calls are printed for it to execute.
    function asAdmin(HoloLockVault vault, bytes[] memory calls) internal {
        address admin = vault.admin();
        if (admin.code.length > 0) {
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
    /// the orderbook, paying the vault's token out of HOLO_VAULT_ID, and evaluating,
    /// through the network's deployer, the claim expression that accepts coupons from
    /// `signer` and nothing else.
    function findClaimOrder(ClaimNetwork memory net, HoloLockVault vault, address signer, uint256 fromBlock)
        internal
        returns (OrderV2 memory order, bytes32 orderHash)
    {
        requireSigner(net, signer);
        bytes32[] memory topics = new bytes32[](1);
        topics[0] = ADD_ORDER;
        Vm.EthGetLogs[] memory logs = vm.eth_getLogs(fromBlock, block.number, address(net.orderbook), topics);
        bool found;
        IExpressionDeployerV3 deployer;
        for (uint256 i = 0; i < logs.length; i++) {
            (address sender, IExpressionDeployerV3 addedBy, OrderV2 memory added, bytes32 addedHash) =
                abi.decode(logs[i].data, (address, IExpressionDeployerV3, OrderV2, bytes32));
            if (sender != address(vault)) continue;
            if (found) {
                require(
                    !net.orderbook.orderExists(orderHash),
                    string.concat("the vault holds another order added since block ", vm.toString(fromBlock))
                );
            }
            (order, orderHash, deployer, found) = (added, addedHash, addedBy, true);
        }
        require(found, string.concat("the vault added no order from block ", vm.toString(fromBlock)));
        ExpressionDeployerParts parts = ExpressionDeployerParts(address(net.deployer));
        require(
            address(deployer) == address(net.deployer) && address(order.evaluable.interpreter) == parts.iInterpreter()
                && address(order.evaluable.store) == parts.iStore(),
            "the claim order was not deployed by the network's expression deployer"
        );
        require(order.hash() == orderHash, "the AddOrder event's order does not hash to its order hash");
        require(net.orderbook.orderExists(orderHash), "the vault's last claim order is no longer on the orderbook");
        require(
            claimOrder(net, address(vault), address(vault.token()), order.evaluable).hash() == orderHash,
            "the vault's last order is not a claim order paying its token out of HOLO_VAULT_ID"
        );

        // A data contract holds a zero byte, then the expression's constants and
        // bytecode, each after its length.
        (bytes memory bytecode, uint256[] memory constants) = parseClaim(net, signer);
        bytes memory expected = abi.encodePacked(bytes1(0), constants.length, constants, bytecode.length, bytecode);
        require(
            keccak256(order.evaluable.expression.code) == keccak256(expected),
            "the claim order's expression is not the claim expression for VALID_SIGNER"
        );
    }

    /// The values the orchestrator and the website take, under their names.
    function printRecord(
        ClaimNetwork memory net,
        HoloLockVault vault,
        address signer,
        OrderV2 memory order,
        bytes32 orderHash
    ) internal view {
        string memory vaultAddress = vm.toString(address(vault));
        string memory token = vm.toString(order.validOutputs[0].token);
        string memory orderbook = vm.toString(address(vault.orderbook()));
        string memory hash = vm.toString(orderHash);
        console2.log("Deploy record");
        console2.log("bridge-orchestrator:");
        console2.log(string.concat("NETWORK=", net.name));
        console2.log(string.concat(net.vaultVar, "=", vaultAddress));
        console2.log(string.concat("TOKEN_ADDRESS=", token));
        console2.log(string.concat("ORDERBOOK_ADDRESS=", orderbook));
        console2.log(string.concat("VAULT_ID=", vm.toString(bytes32(order.validOutputs[0].vaultId))));
        console2.log(string.concat("ORDER_HASH=", hash));
        console2.log(string.concat("ORDER_OWNER=", vm.toString(order.owner)));
        console2.log("website:");
        console2.log(string.concat("PUBLIC_NETWORK=", net.name));
        console2.log(string.concat("PUBLIC_TOKEN_ADDRESS=", token));
        console2.log(string.concat("PUBLIC_LOCK_VAULT_ADDRESS=", vaultAddress));
        console2.log(string.concat("PUBLIC_ORDERBOOK_ADDRESS=", orderbook));
        console2.log(string.concat("PUBLIC_CLAIM_ORDER_HASH=", hash));
        console2.log(string.concat("PUBLIC_CLAIM_SIGNER=", vm.toString(signer)));
        console2.log(string.concat("PUBLIC_CLAIM_INTERPRETER=", vm.toString(address(order.evaluable.interpreter))));
        console2.log(string.concat("PUBLIC_CLAIM_STORE=", vm.toString(address(order.evaluable.store))));
        console2.log(string.concat("PUBLIC_CLAIM_EXPRESSION=", vm.toString(order.evaluable.expression)));
        console2.log(string.concat("PUBLIC_CLAIM_INPUT_TOKEN=", vm.toString(order.validInputs[0].token)));
    }
}
