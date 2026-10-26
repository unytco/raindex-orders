// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {Test, Vm, console2} from "forge-std/Test.sol";
import {
    OrderV4,
    IOV2,
    EvaluableV4,
    TakeOrderConfigV4,
    TakeOrdersConfigV5,
    SignedContextV1,
    Float
} from "raindex-interface-0.1.7/src/interface/IRaindexV6.sol";
import {IInterpreterV4} from "rainlang-interface-0.2.9/src/interface/IInterpreterV4.sol";
import {IInterpreterStoreV3} from "rainlang-interface-0.2.9/src/interface/IInterpreterStoreV3.sol";
import {LibDecimalFloat} from "rain-math-float-0.2.1/src/lib/LibDecimalFloat.sol";
import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {
    RAINDEX,
    MAINNET_HOT,
    HOLO_VAULT_ID,
    MIN_LOCK_AMOUNT,
    CLAIM_IO_RATIO,
    CLAIM_ORDER_NONCE
} from "src/Constants.sol";
import {LibCoupon} from "../lib/LibCoupon.sol";
import {SignContext} from "../lib/SignContext.sol";
import {ISafe, LibSafe} from "./Safe.sol";

/// Proofs that test/fork-rehearsal.sh runs against its anvil fork of mainnet,
/// after it has deployed the bridge there. Skipped unless REHEARSAL is set.
abstract contract ForkRehearsal is Test, SignContext {
    using LibDecimalFloat for Float;

    event Lock(address indexed sender, uint256 amount, bytes32 indexed holochainAgent, uint256 lockId);

    bytes32 internal constant AGENT = keccak256("a Unyt agent");
    bytes32 internal constant ADMIN_CHANGED = keccak256("AdminChanged(address,address)");

    HoloLockVault internal vault;
    /// HOT on mainnet; on another fork, the token REHEARSAL_TOKEN names.
    IERC20 internal hot;
    IInterpreterV4 internal interpreter;
    IInterpreterStoreV3 internal store;
    bytes internal bytecode;
    address internal inputToken;
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
        hot = IERC20(vm.envOr("REHEARSAL_TOKEN", address(MAINNET_HOT)));
        vault = HoloLockVault(vm.envAddress("MAINNET_LOCK_VAULT_ADDRESS"));
        orderHash = vm.envBytes32("ORDER_HASH");
        interpreter = IInterpreterV4(vm.envAddress("PUBLIC_CLAIM_INTERPRETER"));
        store = IInterpreterStoreV3(vm.envAddress("PUBLIC_CLAIM_STORE"));
        bytecode = vm.envBytes("PUBLIC_CLAIM_BYTECODE");
        inputToken = vm.envAddress("PUBLIC_CLAIM_INPUT_TOKEN");
        require(keccak256(abi.encode(order())) == orderHash, "the deploy record does not rebuild ORDER_HASH");
    }

    function order() internal view returns (OrderV4 memory) {
        return OrderV4(
            address(vault),
            EvaluableV4(interpreter, store, bytecode),
            claimIO(inputToken),
            claimIO(address(hot)),
            CLAIM_ORDER_NONCE
        );
    }

    function claimIO(address token) private pure returns (IOV2[] memory io) {
        io = new IOV2[](1);
        io[0] = IOV2(token, HOLO_VAULT_ID);
    }

    function fundLocker(uint256 amount) internal {
        deal(address(hot), locker, amount);
        vm.prank(locker);
        hot.approve(address(vault), amount);
    }

    function lock(uint256 amount) internal {
        fundLocker(amount);
        vm.prank(locker);
        vault.lock(amount, AGENT);
    }

    /// The nine context words the orchestrator signs for a withdrawal of `amount`
    /// to `claimer`. The nonce is the keccak of a 39-byte action hash, as the
    /// orchestrator's is, though its last 4 bytes are not a real DHT location.
    function context(uint256 amount, uint256 expiry, uint256 withdrawal) internal view returns (bytes32[] memory) {
        return contextFor(claimer, amount, expiry, withdrawal);
    }

    function contextFor(address recipient, uint256 amount, uint256 expiry, uint256 withdrawal)
        internal
        view
        returns (bytes32[] memory)
    {
        return LibCoupon.words(
            recipient,
            amount,
            expiry,
            orderHash,
            address(vault),
            address(RAINDEX),
            address(hot),
            HOLO_VAULT_ID,
            keccak256(abi.encodePacked(hex"842924", bytes32(withdrawal), bytes4(0)))
        );
    }

    function inAWeek() internal view returns (uint256) {
        return block.timestamp + 7 days;
    }

    /// The claim as the website sends it: takeOrders4 for exactly the coupon's amount.
    function claimConfig(SignedContextV1 memory coupon) internal view returns (TakeOrdersConfigV5 memory) {
        SignedContextV1[] memory signed = new SignedContextV1[](1);
        signed[0] = coupon;
        TakeOrderConfigV4[] memory orders = new TakeOrderConfigV4[](1);
        orders[0] = TakeOrderConfigV4(order(), 0, 0, signed);
        Float amount = Float.wrap(coupon.context[1]);
        return TakeOrdersConfigV5(amount, amount, CLAIM_IO_RATIO, true, orders, "");
    }

    function claim(SignedContextV1 memory coupon) internal {
        TakeOrdersConfigV5 memory config = claimConfig(coupon);
        vm.prank(claimer);
        RAINDEX.takeOrders4(config);
    }

    /// The vault's admin changed once since `fromBlock`: from `bridge`, the
    /// HoloBridgeDeployer, to `admin`, inside the deploy's one transaction.
    function assertHandedOverOnce(uint256 fromBlock, address bridge, address admin) internal {
        bytes32[] memory topics = new bytes32[](1);
        topics[0] = ADMIN_CHANGED;
        Vm.EthGetLogs[] memory logs = vm.eth_getLogs(fromBlock, block.number, address(vault), topics);
        assertEq(logs.length, 1, "AdminChanged events");
        assertEq(address(uint160(uint256(logs[0].topics[1]))), bridge, "from the HoloBridgeDeployer");
        assertEq(address(uint160(uint256(logs[0].topics[2]))), admin, "to the admin");
        assertEq(logs[0].blockNumber, fromBlock, "in the deploy's block");
    }

    function assertOnlyAdmin(address account) internal {
        OrderV4 memory claimOrder = order();
        vm.startPrank(account);
        vm.expectRevert("HoloLockVault: only admin");
        vault.setAdmin(account);
        vm.expectRevert("HoloLockVault: only admin");
        vault.adminWithdraw(1e18, account);
        vm.expectRevert("HoloLockVault: only admin");
        vault.setMinLockAmount(0);
        vm.expectRevert("HoloLockVault: only admin");
        vault.removeOrder(claimOrder);
        vm.stopPrank();
    }

    /// `data` sent to the vault by `safe`, signed by `keys` of its owners.
    function throughSafe(ISafe safe, uint256[] memory keys, bytes memory data) internal returns (bool) {
        bytes memory signatures = LibSafe.sign(vm, keys, LibSafe.transactionHash(safe, address(vault), data));
        return safe.execTransaction(address(vault), 0, data, 0, 0, 0, 0, address(0), payable(address(0)), signatures);
    }

    function assertTwoOfThree(address safe) internal view {
        assertEq(ISafe(safe).getThreshold(), 2);
        assertEq(ISafe(safe).getOwners().length, 3);
    }

    /// The vault's admin Safe acts with two owner signatures and not with one.
    function assertSafeAdministers(ISafe safe, uint256[] memory twoOwnerKeys) internal {
        bytes memory call = abi.encodeCall(HoloLockVault.setMinLockAmount, (3e18));
        uint256 before = vault.minLockAmount();
        uint256[] memory one = new uint256[](1);
        one[0] = twoOwnerKeys[0];
        bytes memory signature = LibSafe.sign(vm, one, LibSafe.transactionHash(safe, address(vault), call));
        vm.expectRevert(bytes("GS020"));
        safe.execTransaction(address(vault), 0, call, 0, 0, 0, 0, address(0), payable(address(0)), signature);
        assertEq(vault.minLockAmount(), before);

        assertTrue(throughSafe(safe, twoOwnerKeys, call));
        assertEq(vault.minLockAmount(), 3e18);
    }
}

/// The bridge as it launches: one Safe of three owners that needs two
/// signatures, created by `deploy-mainnet.sh safe`, is the vault admin from the
/// deploy's one transaction and the claim order's coupon signer.
contract OneSafeRehearsal is ForkRehearsal {
    error InvalidSignature(uint256 i);

    function safe() private view returns (address) {
        return vm.envAddress("BRIDGE_SAFE");
    }

    function safeSigned(bytes32[] memory c, uint256[] memory ownerKeys) private view returns (SignedContextV1 memory) {
        return SignedContextV1(safe(), c, LibSafe.couponSignature(vm, safe(), ownerKeys, LibCoupon.digest(c)));
    }

    function twoOwners() private view returns (uint256[] memory) {
        return vm.envUint("REHEARSAL_BRIDGE_OWNER_KEYS", ",");
    }

    function oneOwner() private view returns (uint256[] memory one) {
        one = new uint256[](1);
        one[0] = vm.envUint("REHEARSAL_BRIDGE_ONE_OWNER_KEY");
    }

    function expectInvalidSignature(SignedContextV1 memory coupon) private {
        lock(100e18);
        TakeOrdersConfigV5 memory config = claimConfig(coupon);
        vm.prank(claimer);
        vm.expectRevert(abi.encodeWithSelector(InvalidSignature.selector, 0));
        RAINDEX.takeOrders4(config);
    }

    function testTheSafeIsTheAdminFromTheDeploysBlockAndTheCouponSigner() external onFork {
        assertEq(vault.admin(), safe());
        assertTwoOfThree(safe());
        assertHandedOverOnce(vm.envUint("REHEARSAL_DEPLOY_BLOCK"), vm.envAddress("HOLO_BRIDGE_DEPLOYER"), safe());
        assertTrue(RAINDEX.orderExists(orderHash));
    }

    function testACouponCarryingTwoOwnerSignaturesClaims() external onFork {
        lock(100e18);
        uint256 before = vault.vaultBalance();

        claim(safeSigned(context(40e18, inAWeek(), 21), twoOwners()));

        assertEq(hot.balanceOf(claimer), 40e18);
        assertEq(vault.vaultBalance(), before - 40e18);
    }

    function testACouponCarryingOneOwnerSignatureFails() external onFork {
        expectInvalidSignature(safeSigned(context(40e18, inAWeek(), 22), oneOwner()));
    }

    function testACouponCarryingAnOwnersAndAStrangersSignatureFails() external onFork {
        (, uint256 stranger) = makeAddrAndKey("not an owner");
        uint256[] memory keys = new uint256[](2);
        (keys[0], keys[1]) = (oneOwner()[0], stranger);
        expectInvalidSignature(safeSigned(context(40e18, inAWeek(), 23), keys));
    }

    function testACouponCarryingOneOwnersSignatureTwiceFails() external onFork {
        uint256[] memory keys = new uint256[](2);
        (keys[0], keys[1]) = (oneOwner()[0], oneOwner()[0]);
        expectInvalidSignature(safeSigned(context(40e18, inAWeek(), 24), keys));
    }

    function testASecondClaimWithTheSameNonceFails() external onFork {
        lock(100e18);
        claim(safeSigned(context(40e18, inAWeek(), 25), twoOwners()));

        TakeOrdersConfigV5 memory again = claimConfig(safeSigned(context(40e18, inAWeek() + 1, 25), twoOwners()));
        vm.prank(claimer);
        vm.expectRevert(bytes("Nonce already used"));
        RAINDEX.takeOrders4(again);
    }

    function testAnExpiredCouponFails() external onFork {
        lock(100e18);
        SignedContextV1 memory coupon = safeSigned(context(40e18, block.timestamp + 60, 26), twoOwners());
        vm.warp(block.timestamp + 60);

        vm.expectRevert(bytes("Order expired"));
        claim(coupon);
    }

    /// The owners' signatures of a coupon are over its SafeMessage hash, not
    /// over any Safe transaction's hash: they execute no admin call.
    function testACouponsOwnerSignaturesDoNotExecuteAnAdminCall() external onFork {
        bytes memory call = abi.encodeCall(HoloLockVault.adminWithdraw, (1e18, claimer));
        bytes memory signatures = safeSigned(context(40e18, inAWeek(), 27), twoOwners()).signature;
        lock(10e18);

        vm.expectRevert(bytes("GS026"));
        ISafe(safe()).execTransaction(address(vault), 0, call, 0, 0, 0, 0, address(0), payable(address(0)), signatures);
        assertEq(hot.balanceOf(claimer), 0);
    }

    function testTheSafeAdministersWithTwoOwnerSignaturesAndNotWithOne() external onFork {
        assertSafeAdministers(ISafe(safe()), twoOwners());
    }

    function testNeitherTheDeployerNorTheDeployerContractNorAnOwnerAloneAdministers() external onFork {
        lock(10e18);
        assertOnlyAdmin(vm.envAddress("REHEARSAL_DEPLOYER"));
        assertOnlyAdmin(vm.envAddress("HOLO_BRIDGE_DEPLOYER"));
        address[] memory owners = ISafe(safe()).getOwners();
        for (uint256 i = 0; i < owners.length; i++) {
            assertOnlyAdmin(owners[i]);
        }
    }
}

/// The bridge as deploy-mainnet.sh launches it: a Safe of three owners that
/// needs two signatures is the vault admin from the deploy's one transaction,
/// and a key is the coupon signer.
contract LaunchRehearsal is ForkRehearsal {
    function signed(bytes32[] memory c) private view returns (SignedContextV1 memory) {
        return signContext(vm.envUint("REHEARSAL_SIGNER_KEY"), c);
    }

    function testTheVaultHoldsWhatTheDeployRecordSays() external onFork {
        assertEq(address(vault.token()), address(hot));
        assertEq(address(vault.orderbook()), address(RAINDEX));
        assertEq(vault.vaultId(), HOLO_VAULT_ID);
        assertEq(vault.minLockAmount(), MIN_LOCK_AMOUNT);
        assertTrue(RAINDEX.orderExists(orderHash));
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

        assertEq(hot.balanceOf(claimer), 40e18);
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

    /// The admin Safe held the vault from the deploy's block: the one
    /// AdminChanged is the HoloBridgeDeployer handing it over in that block.
    function testTheAdminIsTheSafeFromTheDeploysBlockAndNoKeyEverHeldIt() external onFork {
        address safe = vm.envAddress("ADMIN_SAFE");
        assertEq(vault.admin(), safe);
        assertTwoOfThree(safe);
        assertHandedOverOnce(vm.envUint("REHEARSAL_DEPLOY_BLOCK"), vm.envAddress("HOLO_BRIDGE_DEPLOYER"), safe);
    }

    function testNeitherTheDeployerNorTheDeployerContractNorAnOwnerAloneAdministers() external onFork {
        lock(10e18);
        assertOnlyAdmin(vm.envAddress("REHEARSAL_DEPLOYER"));
        assertOnlyAdmin(vm.envAddress("HOLO_BRIDGE_DEPLOYER"));
        address[] memory owners = ISafe(vm.envAddress("ADMIN_SAFE")).getOwners();
        for (uint256 i = 0; i < owners.length; i++) {
            assertOnlyAdmin(owners[i]);
        }
    }

    function testTheAdminSafeActsWithTwoOwnerSignaturesAndNotWithOne() external onFork {
        assertSafeAdministers(ISafe(vm.envAddress("ADMIN_SAFE")), vm.envUint("REHEARSAL_ADMIN_OWNER_KEYS", ","));
    }
}

/// The bridge after the coupon signer's rotation: the vault admin and the
/// coupon signer are each a Safe of three owners that needs two signatures.
contract SafeSignerRehearsal is ForkRehearsal {
    error InvalidSignature(uint256 i);

    function safe() private view returns (address) {
        return vm.envAddress("SIGNER_SAFE");
    }

    function safeSigned(bytes32[] memory c, uint256[] memory ownerKeys) private view returns (SignedContextV1 memory) {
        return SignedContextV1(safe(), c, LibSafe.couponSignature(vm, safe(), ownerKeys, LibCoupon.digest(c)));
    }

    function safeSigned(bytes32[] memory c, string memory ownerKeys) private view returns (SignedContextV1 memory) {
        return safeSigned(c, vm.envUint(ownerKeys, ","));
    }

    function expectInvalidSignature(SignedContextV1 memory coupon) private {
        lock(100e18);
        vm.expectRevert(abi.encodeWithSelector(InvalidSignature.selector, 0));
        claim(coupon);
    }

    function testTheAdminIsASafeWhoseOwnersActedThroughIt() external onFork {
        address admin = vm.envAddress("ADMIN_SAFE");
        assertEq(vault.admin(), admin);
        assertTwoOfThree(admin);
        assertEq(vault.minLockAmount(), vm.envUint("REHEARSAL_MIN_LOCK_AMOUNT"));
        assertOnlyAdmin(ISafe(admin).getOwners()[0]);
    }

    function testTheRotationReplacedTheClaimOrder() external onFork {
        assertTwoOfThree(safe());
        assertTrue(RAINDEX.orderExists(orderHash));
        assertFalse(RAINDEX.orderExists(vm.envBytes32("PREVIOUS_ORDER_HASH")));
    }

    function testACouponCarryingTwoOwnerSignaturesClaims() external onFork {
        lock(100e18);
        uint256 before = vault.vaultBalance();

        claim(safeSigned(context(40e18, inAWeek(), 5), "REHEARSAL_SIGNER_OWNER_KEYS"));

        assertEq(hot.balanceOf(claimer), 40e18);
        assertEq(vault.vaultBalance(), before - 40e18);
    }

    function testACouponCarryingOneOwnerSignatureFails() external onFork {
        expectInvalidSignature(safeSigned(context(40e18, inAWeek(), 6), "REHEARSAL_SIGNER_ONE_OWNER_KEY"));
    }

    function testACouponCarryingAnOwnersAndANonOwnersSignatureFails() external onFork {
        (, uint256 stranger) = makeAddrAndKey("not an owner");
        uint256[] memory keys = new uint256[](2);
        (keys[0], keys[1]) = (vm.envUint("REHEARSAL_SIGNER_ONE_OWNER_KEY"), stranger);

        expectInvalidSignature(safeSigned(context(40e18, inAWeek(), 7), keys));
    }

    function testACouponCarryingOneOwnersSignatureTwiceFails() external onFork {
        uint256[] memory keys = new uint256[](2);
        keys[0] = vm.envUint("REHEARSAL_SIGNER_ONE_OWNER_KEY");
        keys[1] = keys[0];

        expectInvalidSignature(safeSigned(context(40e18, inAWeek(), 8), keys));
    }

    function testTheOwnersOfTheAdminSafeCannotSignForTheSignerSafe() external onFork {
        expectInvalidSignature(safeSigned(context(40e18, inAWeek(), 9), "REHEARSAL_ADMIN_OWNER_KEYS"));
    }

    function testACouponFromThePreviousSignerFails() external onFork {
        lock(100e18);

        vm.expectRevert(bytes("Wrong signer"));
        claim(signContext(vm.envUint("REHEARSAL_SIGNER_KEY"), context(40e18, inAWeek(), 10)));
    }

    /// Coupons for the website's coupon-status proof, in the form the
    /// orchestrator writes them, paying REHEARSAL_WEBSITE_CLAIMER, and the
    /// claim of the first as calldata for it to send. The first is the one the
    /// Safe accepts; the other two it refuses.
    function testPrintsCouponsForTheWebsite() external onFork {
        address recipient = vm.envAddress("REHEARSAL_WEBSITE_CLAIMER");
        bytes32[] memory c = contextFor(recipient, 40e18, inAWeek(), 100);
        SignedContextV1 memory accepted = safeSigned(c, "REHEARSAL_SIGNER_OWNER_KEYS");
        (, uint256 stranger) = makeAddrAndKey("not an owner");
        uint256[] memory keys = new uint256[](2);
        (keys[0], keys[1]) = (vm.envUint("REHEARSAL_SIGNER_ONE_OWNER_KEY"), stranger);
        console2.log(string.concat("SAFE_COUPON=", serialized(accepted)));
        console2.log(string.concat("ONE_OWNER_COUPON=", serialized(safeSigned(c, "REHEARSAL_SIGNER_ONE_OWNER_KEY"))));
        console2.log(string.concat("NON_OWNER_COUPON=", serialized(safeSigned(c, keys))));
        console2.log(
            string.concat("CLAIM_CALLDATA=", vm.toString(abi.encodeCall(RAINDEX.takeOrders4, (claimConfig(accepted)))))
        );
        lock(100e18);
        TakeOrdersConfigV5 memory config = claimConfig(accepted);
        vm.prank(recipient);
        RAINDEX.takeOrders4(config);
        assertEq(hot.balanceOf(recipient), 40e18);
    }

    /// `signer,signature,c0..c8`, the words as decimal numbers.
    function serialized(SignedContextV1 memory coupon) private pure returns (string memory text) {
        text = string.concat(vm.toString(coupon.signer), ",", vm.toString(coupon.signature));
        for (uint256 i = 0; i < coupon.context.length; i++) {
            text = string.concat(text, ",", vm.toString(uint256(coupon.context[i])));
        }
    }
}

/// The bridge deployed with UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes: a key is the
/// vault admin, and the signer Safe is the coupon signer from the deploy.
contract SingleKeyAdminRehearsal is ForkRehearsal {
    function testTheAdminIsTheKeyFromTheDeploysBlock() external onFork {
        address admin = vm.envAddress("ADMIN_ADDRESS");
        assertEq(vault.admin(), admin);
        // A key, with or without an EIP-7702 delegation, as anvil's accounts carry one on mainnet.
        assertTrue(admin.code.length == 0 || (admin.code.length == 23 && admin.code[0] == 0xef), "the admin is a key");
        assertHandedOverOnce(vm.envUint("REHEARSAL_DEPLOY_BLOCK"), vm.envAddress("HOLO_BRIDGE_DEPLOYER"), admin);
        assertOnlyAdmin(vm.envAddress("REHEARSAL_DEPLOYER"));
        assertOnlyAdmin(vm.envAddress("HOLO_BRIDGE_DEPLOYER"));
    }

    function testACouponCarryingTwoOwnerSignaturesOfTheSignerSafeClaims() external onFork {
        lock(100e18);
        address safe = vm.envAddress("SIGNER_SAFE");
        bytes32[] memory c = context(40e18, inAWeek(), 11);
        uint256[] memory keys = vm.envUint("REHEARSAL_SIGNER_OWNER_KEYS", ",");

        claim(SignedContextV1(safe, c, LibSafe.couponSignature(vm, safe, keys, LibCoupon.digest(c))));

        assertEq(hot.balanceOf(claimer), 40e18);
    }
}
