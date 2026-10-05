// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Test} from "forge-std/Test.sol";
import {ECDSA} from "openzeppelin-contracts/contracts/utils/cryptography/ECDSA.sol";
import {OrderV2, IO, TakeOrderConfigV2, TakeOrdersConfigV2} from "rain.orderbook.interface/interface/IOrderBookV3.sol";
import {EvaluableV2, SignedContextV1} from "rain.interpreter.interface/interface/IInterpreterCallerV2.sol";
import {IInterpreterV2} from "rain.interpreter.interface/interface/IInterpreterV2.sol";
import {IInterpreterStoreV2} from "rain.interpreter.interface/interface/IInterpreterStoreV2.sol";
import {LibOrder} from "rain.orderbook/src/lib/LibOrder.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {MAINNET_ORDERBOOK, MAINNET_HOT, MAINNET_USDT, HOLO_VAULT_ID, MIN_LOCK_AMOUNT} from "src/Constants.sol";
import {SignContext} from "../lib/SignContext.sol";
import {ISafe, LibSafe} from "./Safe.sol";

/// Proofs that test/fork-rehearsal.sh runs against its anvil fork of mainnet,
/// after it has deployed the bridge there. Skipped unless REHEARSAL is set.
abstract contract ForkRehearsal is Test, SignContext {
    using LibOrder for OrderV2;

    event Lock(address indexed sender, uint256 amount, bytes32 indexed holochainAgent, uint256 lockId);

    bytes32 internal constant AGENT = keccak256("a Unyt agent");

    HoloLockVault internal vault;
    EvaluableV2 internal evaluable;
    bytes32 internal orderHash;
    address internal locker = makeAddr("locker");
    address internal claimer = makeAddr("claimer");

    modifier onFork() {
        vm.skip(!vm.envOr("REHEARSAL", false));
        _;
    }

    function setUp() public {
        if (!vm.envOr("REHEARSAL", false)) return;
        vm.createSelectFork(vm.envString("REHEARSAL_RPC_URL"));
        vault = HoloLockVault(vm.envAddress("MAINNET_LOCK_VAULT_ADDRESS"));
        orderHash = vm.envBytes32("ORDER_HASH");
        evaluable = EvaluableV2(
            IInterpreterV2(vm.envAddress("PUBLIC_CLAIM_INTERPRETER")),
            IInterpreterStoreV2(vm.envAddress("PUBLIC_CLAIM_STORE")),
            vm.envAddress("PUBLIC_CLAIM_EXPRESSION")
        );
        require(order().hash() == orderHash, "the deploy record does not rebuild ORDER_HASH");
    }

    function order() internal view returns (OrderV2 memory) {
        return OrderV2(address(vault), true, evaluable, claimIO(address(MAINNET_USDT)), claimIO(address(MAINNET_HOT)));
    }

    function claimIO(address token) private pure returns (IO[] memory io) {
        io = new IO[](1);
        io[0] = IO(token, 18, HOLO_VAULT_ID);
    }

    function fundLocker(uint256 amount) internal {
        deal(address(MAINNET_HOT), locker, amount);
        vm.prank(locker);
        MAINNET_HOT.approve(address(vault), amount);
    }

    function lock(uint256 amount) internal {
        fundLocker(amount);
        vm.prank(locker);
        vault.lock(amount, AGENT);
    }

    /// The nine context words the orchestrator signs for a withdrawal of `amount`
    /// to `claimer`, its nonce the keccak of the withdrawal's 39-byte action hash.
    function context(uint256 amount, uint256 expiry, uint256 withdrawal) internal view returns (uint256[] memory c) {
        c = new uint256[](9);
        c[0] = uint256(uint160(claimer));
        c[1] = amount;
        c[2] = expiry;
        c[3] = uint256(orderHash);
        c[4] = uint256(uint160(address(vault)));
        c[5] = uint256(uint160(address(MAINNET_ORDERBOOK)));
        c[6] = uint256(uint160(address(MAINNET_HOT)));
        c[7] = HOLO_VAULT_ID;
        c[8] = uint256(keccak256(abi.encodePacked(hex"842924", bytes32(withdrawal), bytes4(0))));
    }

    function inAWeek() internal view returns (uint256) {
        return block.timestamp + 7 days;
    }

    function claim(SignedContextV1 memory coupon) internal {
        SignedContextV1[] memory signed = new SignedContextV1[](1);
        signed[0] = coupon;
        TakeOrderConfigV2[] memory orders = new TakeOrderConfigV2[](1);
        orders[0] = TakeOrderConfigV2(order(), 0, 0, signed);
        uint256 amount = coupon.context[1];
        vm.prank(claimer);
        MAINNET_ORDERBOOK.takeOrders(TakeOrdersConfigV2(amount, amount, 0, orders, ""));
    }
}

/// The bridge as deployed: an EOA admin and an EOA coupon signer.
contract EoaRehearsal is ForkRehearsal {
    function signed(uint256[] memory c) private view returns (SignedContextV1 memory) {
        return signContext(vm.envUint("REHEARSAL_SIGNER_KEY"), c);
    }

    function testTheVaultHoldsWhatTheDeployRecordSays() external onFork {
        assertEq(address(vault.token()), address(MAINNET_HOT));
        assertEq(address(vault.orderbook()), address(MAINNET_ORDERBOOK));
        assertEq(vault.vaultId(), HOLO_VAULT_ID);
        assertEq(vault.minLockAmount(), MIN_LOCK_AMOUNT);
        assertTrue(MAINNET_ORDERBOOK.orderExists(orderHash));
    }

    function testALockLandsInTheVaultAndEmitsLock() external onFork {
        uint256 before = vault.vaultBalance();
        uint256 lockId = vault.lockNonce();

        fundLocker(250e18);
        vm.expectEmit(true, true, false, true, address(vault));
        emit Lock(locker, 250e18, AGENT, lockId);
        vm.prank(locker);
        vault.lock(250e18, AGENT);

        assertEq(vault.vaultBalance(), before + 250e18);
    }

    function testACouponSignedAsTheOrchestratorSignsClaimsTheLockedHot() external onFork {
        lock(100e18);
        uint256 before = vault.vaultBalance();

        claim(signed(context(40e18, inAWeek(), 1)));

        assertEq(MAINNET_HOT.balanceOf(claimer), 40e18);
        assertEq(vault.vaultBalance(), before - 40e18);
    }

    function testASecondClaimWithTheSameNonceFails() external onFork {
        lock(100e18);
        claim(signed(context(40e18, inAWeek(), 2)));

        vm.expectRevert(bytes("Nonce already used"));
        claim(signed(context(40e18, inAWeek() + 1, 2)));
    }

    function testACouponFromAnotherSignerFails() external onFork {
        lock(100e18);
        (, uint256 otherKey) = makeAddrAndKey("not the coupon signer");

        vm.expectRevert(bytes("Wrong signer"));
        claim(signContext(otherKey, context(40e18, inAWeek(), 3)));
    }

    function testAnExpiredCouponFails() external onFork {
        lock(100e18);
        SignedContextV1 memory coupon = signed(context(40e18, block.timestamp + 60, 4));
        vm.warp(block.timestamp + 60);

        vm.expectRevert(bytes("Order expired"));
        claim(coupon);
    }

    function testTheAdminIsAdminAddressAndTheDeployerHoldsNoAdminRight() external onFork {
        assertEq(vault.admin(), vm.envAddress("ADMIN_ADDRESS"));
        address deployer = vm.envAddress("REHEARSAL_DEPLOYER");
        lock(10e18);

        vm.startPrank(deployer);
        vm.expectRevert("HoloLockVault: only admin");
        vault.setAdmin(deployer);
        vm.expectRevert("HoloLockVault: only admin");
        vault.adminWithdraw(10e18, deployer);
        vm.expectRevert("HoloLockVault: only admin");
        vault.setMinLockAmount(0);
        vm.expectRevert("HoloLockVault: only admin");
        vault.removeOrder(order());
        vm.stopPrank();
    }
}

/// The bridge after both switches: the vault admin and the coupon signer are
/// each a Safe of three owners that needs two signatures.
contract SafeRehearsal is ForkRehearsal {
    error InvalidSignature(uint256 i);

    function safeSigned(uint256[] memory c, string memory ownerKeys) private view returns (SignedContextV1 memory) {
        address safe = vm.envAddress("SIGNER_SAFE");
        bytes32 digest = ECDSA.toEthSignedMessageHash(keccak256(abi.encodePacked(c)));
        return SignedContextV1(safe, c, LibSafe.couponSignature(vm, safe, vm.envUint(ownerKeys, ","), digest));
    }

    function assertTwoOfThree(address safe) private view {
        assertEq(ISafe(safe).getThreshold(), 2);
        assertEq(ISafe(safe).getOwners().length, 3);
    }

    function testTheAdminIsASafeWhoseOwnersActedThroughIt() external onFork {
        address safe = vm.envAddress("ADMIN_SAFE");
        assertEq(vault.admin(), safe);
        assertTwoOfThree(safe);
        assertEq(vault.minLockAmount(), vm.envUint("REHEARSAL_MIN_LOCK_AMOUNT"));

        vm.prank(vm.envAddress("ADMIN_ADDRESS"));
        vm.expectRevert("HoloLockVault: only admin");
        vault.setMinLockAmount(0);
    }

    function testTheRotationReplacedTheClaimOrder() external onFork {
        assertTwoOfThree(vm.envAddress("SIGNER_SAFE"));
        assertTrue(MAINNET_ORDERBOOK.orderExists(orderHash));
        assertFalse(MAINNET_ORDERBOOK.orderExists(vm.envBytes32("PREVIOUS_ORDER_HASH")));
    }

    function testACouponCarryingTwoOwnerSignaturesClaims() external onFork {
        lock(100e18);
        uint256 before = vault.vaultBalance();

        claim(safeSigned(context(40e18, inAWeek(), 5), "REHEARSAL_SIGNER_OWNER_KEYS"));

        assertEq(MAINNET_HOT.balanceOf(claimer), 40e18);
        assertEq(vault.vaultBalance(), before - 40e18);
    }

    function testACouponCarryingOneOwnerSignatureFails() external onFork {
        lock(100e18);

        SignedContextV1 memory coupon = safeSigned(context(40e18, inAWeek(), 6), "REHEARSAL_SIGNER_ONE_OWNER_KEY");

        vm.expectRevert(abi.encodeWithSelector(InvalidSignature.selector, 0));
        claim(coupon);
    }

    function testTheOwnersOfAnotherSafeCannotSignForIt() external onFork {
        lock(100e18);

        SignedContextV1 memory coupon = safeSigned(context(40e18, inAWeek(), 7), "REHEARSAL_ADMIN_OWNER_KEYS");

        vm.expectRevert(abi.encodeWithSelector(InvalidSignature.selector, 0));
        claim(coupon);
    }

    function testACouponFromThePreviousSignerFails() external onFork {
        lock(100e18);

        vm.expectRevert(bytes("Wrong signer"));
        claim(signContext(vm.envUint("REHEARSAL_SIGNER_KEY"), context(40e18, inAWeek(), 8)));
    }
}
