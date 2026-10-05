// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {OrderV2} from "rain.orderbook.interface/interface/IOrderBookV3.sol";
import {EvaluableV2} from "rain.interpreter.interface/interface/IInterpreterCallerV2.sol";
import {IInterpreterV2} from "rain.interpreter.interface/interface/IInterpreterV2.sol";
import {IInterpreterStoreV2} from "rain.interpreter.interface/interface/IInterpreterStoreV2.sol";
import {LibOrder} from "rain.orderbook/src/lib/LibOrder.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {ClaimOrderScript, ClaimNetwork} from "./ClaimOrderScript.sol";

/// Replaces the vault's claim order with one that accepts coupons from
/// VALID_SIGNER: adds the new order, then removes the current one. It reads the
/// current one from the deploy record's variables, under the names the record
/// prints them. rotate-claim-signer.sh runs it.
contract RotateClaimSigner is ClaimOrderScript {
    using LibOrder for OrderV2;

    function run() external {
        ClaimNetwork memory net = networkFromEnv();
        HoloLockVault vault = HoloLockVault(vm.envAddress(net.vaultVar));
        address signer = vm.envAddress("VALID_SIGNER");
        requireSigner(net, signer);
        require(
            address(vault.orderbook()) == address(net.orderbook), "the vault's orderbook is not NETWORK's OrderBookV3"
        );

        EvaluableV2 memory evaluable = EvaluableV2(
            IInterpreterV2(vm.envAddress("PUBLIC_CLAIM_INTERPRETER")),
            IInterpreterStoreV2(vm.envAddress("PUBLIC_CLAIM_STORE")),
            vm.envAddress("PUBLIC_CLAIM_EXPRESSION")
        );
        OrderV2 memory current = claimOrder(net, address(vault), address(vault.token()), evaluable);
        bytes32 currentHash = current.hash();
        require(
            currentHash == vm.envBytes32("ORDER_HASH"),
            "PUBLIC_CLAIM_INTERPRETER, PUBLIC_CLAIM_STORE and PUBLIC_CLAIM_EXPRESSION do not rebuild ORDER_HASH"
        );
        require(net.orderbook.orderExists(currentHash), "ORDER_HASH is not an order on the orderbook");

        bytes[] memory calls = new bytes[](2);
        calls[0] = abi.encodeCall(HoloLockVault.addOrder, (claimOrderConfig(net, address(vault.token()), signer)));
        calls[1] = abi.encodeCall(HoloLockVault.removeOrder, (current));
        asAdmin(vault, calls);
    }

    /// Prints the deploy record of the claim order the vault added from
    /// `fromBlock` on, once the previous one, ORDER_HASH, is removed.
    function record(uint256 fromBlock) external {
        ClaimNetwork memory net = networkFromEnv();
        HoloLockVault vault = HoloLockVault(vm.envAddress(net.vaultVar));
        address signer = vm.envAddress("VALID_SIGNER");
        (OrderV2 memory order, bytes32 orderHash) = findClaimOrder(net, vault, signer, fromBlock);
        require(
            !net.orderbook.orderExists(vm.envBytes32("ORDER_HASH")),
            "the previous claim order ORDER_HASH is still on the orderbook"
        );
        printRecord(net, vault, signer, order, orderHash);
    }
}
