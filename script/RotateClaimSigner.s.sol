// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {OrderV2} from "rain.orderbook.interface/interface/IOrderBookV3.sol";
import {EvaluableV2} from "rain.interpreter.interface/interface/IInterpreterCallerV2.sol";
import {IInterpreterV2} from "rain.interpreter.interface/interface/IInterpreterV2.sol";
import {IInterpreterStoreV2} from "rain.interpreter.interface/interface/IInterpreterStoreV2.sol";
import {LibOrder} from "rain.orderbook/src/lib/LibOrder.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {ClaimOrderScript, ClaimNetwork} from "./ClaimOrderScript.sol";

/// @title RotateClaimSigner
/// @notice Replaces the claim order of LOCK_VAULT_ADDRESS with one that accepts
/// coupons from VALID_SIGNER: adds the new order, then removes the one named by
/// ORDER_HASH and the CLAIM_INTERPRETER, CLAIM_STORE and CLAIM_EXPRESSION of the
/// deploy record. Run it through rotate-claim-signer.sh.
contract RotateClaimSigner is ClaimOrderScript {
    using LibOrder for OrderV2;

    function run() external {
        ClaimNetwork memory net = networkFromEnv();
        HoloLockVault vault = HoloLockVault(vm.envAddress("LOCK_VAULT_ADDRESS"));
        address signer = vm.envAddress("VALID_SIGNER");
        requireSigner(net, signer);
        require(
            address(vault.orderbook()) == address(net.orderbook), "the vault's orderbook is not NETWORK's OrderBookV3"
        );

        EvaluableV2 memory evaluable = EvaluableV2(
            IInterpreterV2(vm.envAddress("CLAIM_INTERPRETER")),
            IInterpreterStoreV2(vm.envAddress("CLAIM_STORE")),
            vm.envAddress("CLAIM_EXPRESSION")
        );
        OrderV2 memory current = claimOrder(net, address(vault), address(vault.token()), evaluable);
        bytes32 currentHash = current.hash();
        require(
            currentHash == vm.envBytes32("ORDER_HASH"),
            "CLAIM_INTERPRETER, CLAIM_STORE and CLAIM_EXPRESSION do not rebuild ORDER_HASH"
        );
        require(net.orderbook.orderExists(currentHash), "ORDER_HASH is not an order on the orderbook");

        bytes[] memory calls = new bytes[](2);
        calls[0] = abi.encodeCall(HoloLockVault.addOrder, (claimOrderConfig(net, address(vault.token()), signer)));
        calls[1] = abi.encodeCall(HoloLockVault.removeOrder, (current));
        asAdmin(vault, calls);
    }

    /// Prints the deploy record of the claim order the vault added last from
    /// `fromBlock` on, once the rotation has executed.
    function record(uint256 fromBlock) external {
        ClaimNetwork memory net = networkFromEnv();
        HoloLockVault vault = HoloLockVault(vm.envAddress("LOCK_VAULT_ADDRESS"));
        (OrderV2 memory order, bytes32 orderHash) = findClaimOrder(net, vault, fromBlock);
        require(
            !net.orderbook.orderExists(vm.envBytes32("ORDER_HASH")),
            "the previous claim order ORDER_HASH is still on the orderbook"
        );
        printRecord(net, vault, vm.envAddress("VALID_SIGNER"), order, orderHash);
    }
}
