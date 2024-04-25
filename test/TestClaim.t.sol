// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {console2, Test, Vm} from "forge-std/Test.sol";
import {IOrderBookV3ArbOrderTaker} from "rain.orderbook.interface/interface/IOrderBookV3ArbOrderTaker.sol";
import {IParserV1} from "rain.interpreter.interface/interface/IParserV1.sol";
import {IExpressionDeployerV3} from "rain.interpreter.interface/interface/IExpressionDeployerV3.sol";
import {EvaluableConfigV3, SignedContextV1} from "rain.interpreter.interface/interface/IInterpreterCallerV2.sol";
import {LibComposeOrders} from "./lib/LibComposeOrder.sol";
import {
    OrderConfigV2,
    OrderV2,
    IO,
    TakeOrderConfigV2,
    TakeOrdersConfigV2
} from "rain.orderbook.interface/interface/IOrderBookV3.sol";
import {LibOrder} from "rain.orderbook/src/lib/LibOrder.sol";
import {SignContext} from "./lib/SignContext.sol";
import {
    TEST_SIGNER_KEY,
    TEST_SIGNER_ADDRESS,
    MAINNET_ORDERBOOK,
    MAINNET_DEPLOYER,
    MAINNET_HOT,
    MAINNET_USDT
} from "src/Constants.sol";

interface GetParser {
    function iParser() external view returns (IParserV1);
}

contract TestClaim is Test, SignContext {
    using LibOrder for OrderV2;

    function setUp() public {
        uint256 fork = vm.createFork(vm.envString("RPC_URL_ETH"));
        vm.selectFork(fork);
    }

    function testHappyPath() public {
        address orderOwner = makeAddr("owner");
        address taker = makeAddr("taker");

        bytes memory rainlang = LibComposeOrders.getComposedOrder(
            vm, "src/holo-claim.rain", "mainnet", "./lib/rain.orderbook", "./lib/rain.orderbook/Cargo.toml"
        );
        console2.log(string(rainlang));

        IParserV1 parser = GetParser(address(MAINNET_DEPLOYER)).iParser();

        (bytes memory bytecode, uint256[] memory constants) = parser.parse(rainlang);

        OrderV2 memory order;

        {
            EvaluableConfigV3 memory evaluableConfig = EvaluableConfigV3(MAINNET_DEPLOYER, bytecode, constants);
            IO[] memory inputs = new IO[](1);
            inputs[0] = IO(address(MAINNET_USDT), 6, 1);
            IO[] memory outputs = new IO[](1);
            outputs[0] = IO(address(MAINNET_HOT), 18, 1);

            OrderConfigV2 memory orderConfig = OrderConfigV2(inputs, outputs, evaluableConfig, "");

            vm.startPrank(orderOwner);
            vm.recordLogs();
            MAINNET_ORDERBOOK.addOrder(orderConfig);
            Vm.Log[] memory entries = vm.getRecordedLogs();
            (,, order,) = abi.decode(entries[2].data, (address, address, OrderV2, bytes32));
        }

        {
            deal(address(MAINNET_HOT), orderOwner, 100e18);
            vm.startPrank(orderOwner);
            MAINNET_HOT.approve(address(MAINNET_ORDERBOOK), 100e18);
            MAINNET_ORDERBOOK.deposit(address(MAINNET_HOT), 1, 100e18);
            assertEq(MAINNET_HOT.balanceOf(address(MAINNET_ORDERBOOK)), 100e18);
        }

        uint256 privateKey = uint256(TEST_SIGNER_KEY);

        uint256 amount = 100e18;

        {
            /**
             *  Our "coupon" (the SignedContext array) will be:
             *  [0] recipient address
             *  [1] amount
             *  [2] expiry timestamp in seconds
             *  Plus some domain separators
             *  [3] order hash
             *  [4] order owner
             *  [5] orderbook address
             *  [6] token address
             *  [7] output vault id
             */
            uint256[] memory context = new uint256[](9);

            context[0] = uint256(uint160(taker));
            context[1] = amount;
            context[2] = block.timestamp + 60 * 60 * 24 * 7; // 1 week
            context[3] = uint256(order.hash());
            context[4] = uint256(uint160(orderOwner));
            context[5] = uint256(uint160(address(MAINNET_ORDERBOOK)));
            context[6] = uint256(uint160(address(MAINNET_HOT)));
            context[7] = 1;
            context[8] = 10;

            SignedContextV1[] memory signedContexts = new SignedContextV1[](1);
            signedContexts[0] = signContext(privateKey, context);

            TakeOrderConfigV2[] memory orders = new TakeOrderConfigV2[](1);

            orders[0] = TakeOrderConfigV2(order, 0, 0, signedContexts);

            TakeOrdersConfigV2 memory takeOrdersConfig = TakeOrdersConfigV2(amount, amount, 0, orders, "");
            vm.startPrank(taker);
            MAINNET_ORDERBOOK.takeOrders(takeOrdersConfig);
        }

        assertEq(MAINNET_HOT.balanceOf(taker), amount);
    }
}
