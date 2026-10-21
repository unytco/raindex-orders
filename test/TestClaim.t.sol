// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {Test, stdError} from "forge-std/Test.sol";
import {
    OrderConfigV4,
    OrderV4,
    IOV2,
    EvaluableV4,
    TaskV2,
    TakeOrderConfigV4,
    TakeOrdersConfigV5,
    SignedContextV1,
    Float
} from "raindex-interface-0.1.7/src/interface/IRaindexV6.sol";
import {LibDecimalFloat} from "rain-math-float-0.2.1/src/lib/LibDecimalFloat.sol";
import {ClaimInputToken} from "src/ClaimInputToken.sol";
import {
    RAINDEX,
    RAINDEX_SUBPARSER,
    RAINLANG_DEPLOYER,
    RAINLANG_INTERPRETER,
    RAINLANG_STORE,
    MAINNET_HOT,
    HOLO_VAULT_ID,
    CLAIM_IO_RATIO,
    CLAIM_ORDER_NONCE
} from "src/Constants.sol";
import {LibCoupon} from "./lib/LibCoupon.sol";
import {SignContext} from "./lib/SignContext.sol";

/// The claim order on a fork of Ethereum mainnet, against the Raindex V6 and
/// Rainlang contracts Rain deployed there.
contract TestClaim is Test, SignContext {
    using LibDecimalFloat for Float;

    address internal orderOwner = makeAddr("owner");
    address internal claimer = makeAddr("claimer");
    address internal signer;
    uint256 internal signerKey;
    ClaimInputToken internal inputToken;
    bytes internal bytecode;
    bytes32 internal orderHash;

    function setUp() public {
        // FORK_BLOCK_ETH pins the fork to a block (an archive RPC serves an old one);
        // unset, it is the latest.
        string memory rpc = vm.envOr("RPC_URL_ETH", string("https://ethereum-rpc.publicnode.com"));
        uint256 forkBlock = vm.envOr("FORK_BLOCK_ETH", uint256(0));
        if (forkBlock == 0) vm.createSelectFork(rpc);
        else vm.createSelectFork(rpc, forkBlock);

        (signer, signerKey) = makeAddrAndKey("coupon signer");
        inputToken = new ClaimInputToken();

        bytecode = RAINLANG_DEPLOYER.parse2(composed(signer));
        OrderV4 memory claimOrder = order();

        vm.startPrank(orderOwner);
        assertTrue(
            RAINDEX.addOrder4(
                OrderConfigV4(
                    claimOrder.evaluable,
                    claimOrder.validInputs,
                    claimOrder.validOutputs,
                    CLAIM_ORDER_NONCE,
                    bytes32(0),
                    ""
                ),
                new TaskV2[](0)
            )
        );
        orderHash = keccak256(abi.encode(claimOrder));
        assertTrue(RAINDEX.orderExists(orderHash));

        deal(address(MAINNET_HOT), orderOwner, 100e18);
        MAINNET_HOT.approve(address(RAINDEX), 100e18);
        RAINDEX.deposit4(
            address(MAINNET_HOT),
            HOLO_VAULT_ID,
            LibDecimalFloat.fromFixedDecimalLosslessPacked(100e18, 18),
            new TaskV2[](0)
        );
        vm.stopPrank();
        assertEq(vaultBalance(), 100e18);
    }

    /// The claim order as the vault adds it and a claim names it.
    function order() internal view returns (OrderV4 memory) {
        return orderRunning(bytecode);
    }

    /// The claim order running `code`: the one a rotation adds, composed for
    /// another signer, has another order hash on the same owner.
    function orderRunning(bytes memory code) internal view returns (OrderV4 memory) {
        IOV2[] memory inputs = new IOV2[](1);
        inputs[0] = IOV2(address(inputToken), HOLO_VAULT_ID);
        IOV2[] memory outputs = new IOV2[](1);
        outputs[0] = IOV2(address(MAINNET_HOT), HOLO_VAULT_ID);
        return OrderV4(
            orderOwner, EvaluableV4(RAINLANG_INTERPRETER, RAINLANG_STORE, code), inputs, outputs, CLAIM_ORDER_NONCE
        );
    }

    function composed(address validSigner) internal returns (bytes memory) {
        string[] memory compose = new string[](8);
        compose[0] = "node";
        compose[1] = "compose-rainlang.mjs";
        compose[2] = "--network";
        compose[3] = "mainnet";
        compose[4] = "--subparser";
        compose[5] = vm.toString(RAINDEX_SUBPARSER);
        compose[6] = "--signer";
        compose[7] = vm.toString(validSigner);
        return vm.ffi(compose);
    }

    function vaultBalance() internal view returns (uint256 balance) {
        (balance,) = RAINDEX.vaultBalance2(orderOwner, address(MAINNET_HOT), HOLO_VAULT_ID).toFixedDecimalLossy(18);
    }

    function coupon(uint256 amount, uint256 expiry, uint256 nonce) internal view returns (bytes32[] memory) {
        return LibCoupon.words(
            claimer,
            amount,
            expiry,
            orderHash,
            orderOwner,
            address(RAINDEX),
            address(MAINNET_HOT),
            HOLO_VAULT_ID,
            bytes32(nonce)
        );
    }

    function signed(bytes32[] memory words) internal view returns (SignedContextV1 memory) {
        return signContext(signerKey, words);
    }

    function inAWeek() internal view returns (uint256) {
        return block.timestamp + 7 days;
    }

    /// The claim: takeOrders4 for exactly `amount` of HOT, as the website sends it.
    function claim(SignedContextV1 memory signedCoupon, uint256 amount) internal {
        claimOn(order(), signedCoupon, amount);
    }

    /// The claim of `signedCoupon` on `claimOrder`.
    function claimOn(OrderV4 memory claimOrder, SignedContextV1 memory signedCoupon, uint256 amount) internal {
        SignedContextV1[] memory signedContexts = new SignedContextV1[](1);
        signedContexts[0] = signedCoupon;
        TakeOrderConfigV4[] memory orders = new TakeOrderConfigV4[](1);
        orders[0] = TakeOrderConfigV4(claimOrder, 0, 0, signedContexts);
        Float io = LibDecimalFloat.fromFixedDecimalLosslessPacked(amount, 18);
        vm.prank(claimer);
        RAINDEX.takeOrders4(TakeOrdersConfigV5(io, io, CLAIM_IO_RATIO, true, orders, ""));
    }

    function testHappyPath() public {
        claim(signed(coupon(40e18, inAWeek(), 1)), 40e18);

        assertEq(MAINNET_HOT.balanceOf(claimer), 40e18);
        assertEq(vaultBalance(), 60e18);
    }

    /// The claim pays to the wei: the float the coupon carries converts back
    /// to the amount it was made from.
    function testAnOddNumberOfWeiPaysExactly() public {
        uint256 amount = 12345678901234567891;
        claim(signed(coupon(amount, inAWeek(), 2)), amount);

        assertEq(MAINNET_HOT.balanceOf(claimer), amount);
        assertEq(vaultBalance(), 100e18 - amount);
    }

    function testTheClaimerPaysNoInputToken() public {
        claim(signed(coupon(40e18, inAWeek(), 3)), 40e18);

        assertEq(inputToken.balanceOf(claimer), 0);
        assertEq(inputToken.balanceOf(address(RAINDEX)), 0);
    }

    function testACouponFromAnotherSignerFails() public {
        (, uint256 otherKey) = makeAddrAndKey("not the coupon signer");
        SignedContextV1 memory other = signContext(otherKey, coupon(40e18, inAWeek(), 4));

        vm.expectRevert(bytes("Wrong signer"));
        claim(other, 40e18);
    }

    function testTheSameNonceCannotBeClaimedTwice() public {
        claim(signed(coupon(40e18, inAWeek(), 5)), 40e18);
        SignedContextV1 memory again = signed(coupon(40e18, inAWeek() + 1, 5));

        vm.expectRevert(bytes("Nonce already used"));
        claim(again, 40e18);
    }

    /// A rotation replaces the claim order with one composed for another
    /// signer, so the order hash changes. The store keeps the used nonces under
    /// the vault, the order's owner, not under the order: a nonce claimed on the
    /// old order is used on the new one too, so one burn pays once whatever the
    /// signer becomes. A fresh nonce still claims on the new order.
    function testANonceClaimedOnOneClaimOrderIsUsedOnTheNext() public {
        claim(signed(coupon(40e18, inAWeek(), 7)), 40e18);

        (address nextSigner, uint256 nextKey) = makeAddrAndKey("the coupon signer after a rotation");
        OrderV4 memory next = orderRunning(RAINLANG_DEPLOYER.parse2(composed(nextSigner)));
        bytes32 nextHash = keccak256(abi.encode(next));
        assertTrue(nextHash != orderHash);
        vm.startPrank(orderOwner);
        assertTrue(
            RAINDEX.addOrder4(
                OrderConfigV4(next.evaluable, next.validInputs, next.validOutputs, CLAIM_ORDER_NONCE, bytes32(0), ""),
                new TaskV2[](0)
            )
        );
        vm.stopPrank();
        bytes32[] memory sameNonce = LibCoupon.words(
            claimer,
            40e18,
            inAWeek(),
            nextHash,
            orderOwner,
            address(RAINDEX),
            address(MAINNET_HOT),
            HOLO_VAULT_ID,
            bytes32(uint256(7))
        );
        vm.expectRevert(bytes("Nonce already used"));
        claimOn(next, signContext(nextKey, sameNonce), 40e18);

        bytes32[] memory freshNonce = LibCoupon.words(
            claimer,
            40e18,
            inAWeek(),
            nextHash,
            orderOwner,
            address(RAINDEX),
            address(MAINNET_HOT),
            HOLO_VAULT_ID,
            bytes32(uint256(8))
        );
        claimOn(next, signContext(nextKey, freshNonce), 40e18);
        assertEq(MAINNET_HOT.balanceOf(claimer), 80e18);
        assertEq(vaultBalance(), 20e18);
    }

    function testAnExpiredCouponFails() public {
        SignedContextV1 memory expired = signed(coupon(40e18, block.timestamp + 60, 6));
        vm.warp(block.timestamp + 60);

        vm.expectRevert(bytes("Order expired"));
        claim(expired, 40e18);
    }

    function testACouponForAnotherRecipientFails() public {
        bytes32[] memory words = coupon(40e18, inAWeek(), 7);
        words[0] = bytes32(uint256(uint160(makeAddr("someone else"))));
        SignedContextV1 memory other = signed(words);

        vm.expectRevert(bytes("Wrong recipient"));
        claim(other, 40e18);
    }

    function testAClaimForLessThanTheCouponFails() public {
        SignedContextV1 memory forty = signed(coupon(40e18, inAWeek(), 8));

        vm.expectRevert(bytes("Wrong output amount"));
        claim(forty, 30e18);
    }

    function testAClaimForMoreThanTheVaultHoldsFails() public {
        SignedContextV1 memory tooMuch = signed(coupon(140e18, inAWeek(), 9));

        vm.expectRevert(bytes("Wrong output amount"));
        claim(tooMuch, 140e18);
    }

    function testAClaimWithNoCouponFails() public {
        TakeOrderConfigV4[] memory orders = new TakeOrderConfigV4[](1);
        orders[0] = TakeOrderConfigV4(order(), 0, 0, new SignedContextV1[](0));
        Float io = LibDecimalFloat.fromFixedDecimalLosslessPacked(40e18, 18);

        // The order reads the signer, and a claim with no coupon has no signers column.
        // The out-of-bounds panic is the pinned interpreter's, not a contract: it pins
        // that the claim fails there rather than at a later check, and a Rainlang
        // release that answers otherwise shows up here.
        vm.prank(claimer);
        vm.expectRevert(stdError.indexOOBError);
        RAINDEX.takeOrders4(TakeOrdersConfigV5(io, io, CLAIM_IO_RATIO, true, orders, ""));
    }
}
