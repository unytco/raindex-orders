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

interface GetParser {
    function iParser() external view returns (IParserV1);
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

/// Adds, replaces and reads back the claim order a HoloLockVault owns.
abstract contract ClaimOrderScript is Script {
    using LibOrder for OrderV2;

    /// Every IO of the claim order declares 18 decimals, as the website builds the
    /// order struct with 18 for both, and the order's hash covers them.
    uint8 internal constant CLAIM_IO_DECIMALS = 18;

    /// Topic of OrderBookV3's `AddOrder(address, IExpressionDeployerV3, OrderV2, bytes32)`.
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

    /// `NETWORK`'s claim order, refusing an RPC that answers for another chain.
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

    function requireSigner(ClaimNetwork memory net, address signer) internal pure {
        require(signer != address(0), "VALID_SIGNER must be a nonzero address");
        require(
            net.chainId != 1 || signer != TEST_SIGNER_ADDRESS,
            "VALID_SIGNER is the test signer, whose key is public: mainnet refuses it"
        );
    }

    /// The claim order paying `token` out of HOLO_VAULT_ID, accepting coupons
    /// from `signer`. Composed by compose-rainlang.mjs and parsed by the
    /// network's deployer, both before anything is sent.
    function claimOrderConfig(ClaimNetwork memory net, address token, address signer)
        internal
        returns (OrderConfigV2 memory)
    {
        bytes memory rainlang = composeClaim(net, signer);
        (bytes memory bytecode, uint256[] memory constants) = GetParser(address(net.deployer)).iParser().parse(rainlang);
        return OrderConfigV2(
            claimIO(net.inputToken), claimIO(token), EvaluableConfigV3(net.deployer, bytecode, constants), ""
        );
    }

    /// The order the orderbook holds for `vault` after it added the claim order
    /// whose evaluable is `evaluable`.
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

    function composeClaim(ClaimNetwork memory net, address signer) private returns (bytes memory) {
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
        return composed.stdout;
    }

    /// Sends `calls` to the vault as its admin. An admin that is a contract, such
    /// as a Safe, cannot sign here: its calls are printed for it to propose.
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
            (bool ok, bytes memory reason) = address(vault).call(calls[i]);
            if (!ok) {
                assembly ("memory-safe") {
                    revert(add(reason, 0x20), mload(reason))
                }
            }
        }
        vm.stopBroadcast();
    }

    /// The claim order `vault` added last from `fromBlock` on, read from the
    /// chain, and still on the orderbook.
    function findClaimOrder(ClaimNetwork memory net, HoloLockVault vault, uint256 fromBlock)
        internal
        returns (OrderV2 memory order, bytes32 orderHash)
    {
        bytes32[] memory topics = new bytes32[](1);
        topics[0] = ADD_ORDER;
        Vm.EthGetLogs[] memory logs = vm.eth_getLogs(fromBlock, block.number, address(net.orderbook), topics);
        bool found;
        for (uint256 i = 0; i < logs.length; i++) {
            (address sender,, OrderV2 memory added, bytes32 addedHash) =
                abi.decode(logs[i].data, (address, IExpressionDeployerV3, OrderV2, bytes32));
            if (sender == address(vault)) {
                (order, orderHash, found) = (added, addedHash, true);
            }
        }
        require(found, string.concat("the vault added no order from block ", vm.toString(fromBlock)));
        require(order.hash() == orderHash, "the AddOrder event's order does not hash to its order hash");
        require(order.owner == address(vault), "the claim order's owner is not the vault");
        require(net.orderbook.orderExists(orderHash), "the vault's last claim order is no longer on the orderbook");
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
        string memory token = vm.toString(address(vault.token()));
        string memory orderbook = vm.toString(address(net.orderbook));
        string memory hash = vm.toString(orderHash);
        console2.log("Deploy record");
        console2.log("bridge-orchestrator:");
        console2.log(string.concat("NETWORK=", net.name));
        console2.log(string.concat(net.vaultVar, "=", vaultAddress));
        console2.log(string.concat("TOKEN_ADDRESS=", token));
        console2.log(string.concat("ORDERBOOK_ADDRESS=", orderbook));
        console2.log(string.concat("VAULT_ID=", vm.toString(bytes32(HOLO_VAULT_ID))));
        console2.log(string.concat("ORDER_HASH=", hash));
        console2.log(string.concat("ORDER_OWNER=", vaultAddress));
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
        console2.log(string.concat("PUBLIC_CLAIM_INPUT_TOKEN=", vm.toString(net.inputToken)));
    }
}
