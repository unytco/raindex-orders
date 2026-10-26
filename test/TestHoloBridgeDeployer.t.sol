// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {Test, Vm} from "forge-std/Test.sol";
import {
    IRaindexV6,
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
import {HoloLockVault} from "src/HoloLockVault.sol";
import {HoloBridgeDeployer} from "src/HoloBridgeDeployer.sol";
import {MockHOT} from "src/test/MockHOT.sol";
import {MockRaindex} from "src/test/MockRaindex.sol";
import {
    RAINDEX,
    RAINDEX_SUBPARSER,
    RAINLANG_DEPLOYER,
    RAINLANG_INTERPRETER,
    RAINLANG_STORE,
    MAINNET_HOT,
    HOLO_VAULT_ID,
    MIN_LOCK_AMOUNT,
    CLAIM_IO_RATIO,
    CLAIM_ORDER_NONCE
} from "src/Constants.sol";
import {LibCoupon} from "./lib/LibCoupon.sol";
import {SignContext} from "./lib/SignContext.sol";
import {ISafe, LibSafe, SAFE_PROXY_FACTORY, SAFE_SINGLETON} from "./rehearsal/Safe.sol";

/// HoloBridgeDeployer against mocks: what one transaction leaves behind.
contract TestHoloBridgeDeployer is Test {
    event AdminChanged(address indexed oldAdmin, address indexed newAdmin);

    MockHOT internal token = new MockHOT();
    MockRaindex internal orderbook = new MockRaindex();
    address internal admin = makeAddr("admin");
    bytes32 internal constant VAULT_ID = bytes32(uint256(0x1234));

    function evaluable() internal pure returns (EvaluableV4 memory) {
        return EvaluableV4(RAINLANG_INTERPRETER, RAINLANG_STORE, hex"00ab");
    }

    function orderbookAdds(bool added) internal {
        vm.mockCall(address(orderbook), abi.encodeWithSelector(IRaindexV6.addOrder4.selector), abi.encode(added));
    }

    function deployBridge() internal returns (HoloBridgeDeployer) {
        return new HoloBridgeDeployer(
            address(token), address(orderbook), VAULT_ID, MIN_LOCK_AMOUNT, evaluable(), CLAIM_ORDER_NONCE, admin
        );
    }

    function testTheVaultIsAdminsFromItsFirstBlockAndTheDeployerHoldsNoRight() public {
        orderbookAdds(true);
        HoloBridgeDeployer bridge = deployBridge();
        HoloLockVault vault = bridge.vault();

        assertEq(vault.admin(), admin);
        assertEq(address(vault.token()), address(token));
        assertEq(address(vault.orderbook()), address(orderbook));
        assertEq(vault.vaultId(), VAULT_ID);
        assertEq(vault.minLockAmount(), MIN_LOCK_AMOUNT);
        assertEq(bridge.inputToken().decimals(), 18);

        vm.startPrank(address(bridge));
        vm.expectRevert("HoloLockVault: only admin");
        vault.setAdmin(address(bridge));
        vm.expectRevert("HoloLockVault: only admin");
        vault.setMinLockAmount(0);
        vm.stopPrank();
        vm.prank(address(this));
        vm.expectRevert("HoloLockVault: only admin");
        vault.setAdmin(address(this));
    }

    /// The vault's admin is handed over once, from the deployer contract to the
    /// admin: no key is its admin in between.
    function testTheAdminChangesOnceFromTheDeployerContractToTheAdmin() public {
        orderbookAdds(true);
        address bridge = vm.computeCreateAddress(address(this), vm.getNonce(address(this)));
        address vault = vm.computeCreateAddress(bridge, 2);

        vm.recordLogs();
        deployBridge();

        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 adminChanges;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] != AdminChanged.selector) continue;
            adminChanges++;
            assertEq(logs[i].emitter, vault);
            assertEq(address(uint160(uint256(logs[i].topics[1]))), bridge);
            assertEq(address(uint160(uint256(logs[i].topics[2]))), admin);
        }
        assertEq(adminChanges, 1);
    }

    /// The claim order pays the vault's token out of the vault ID against the
    /// input token the deployer deployed, and is added by the vault.
    function testTheClaimOrderIsAddedByTheVaultAgainstTheInputToken() public {
        orderbookAdds(true);
        address bridge = vm.computeCreateAddress(address(this), vm.getNonce(address(this)));
        address inputToken = vm.computeCreateAddress(bridge, 1);
        address vault = vm.computeCreateAddress(bridge, 2);
        IOV2[] memory inputs = new IOV2[](1);
        inputs[0] = IOV2(inputToken, VAULT_ID);
        IOV2[] memory outputs = new IOV2[](1);
        outputs[0] = IOV2(address(token), VAULT_ID);
        OrderConfigV4 memory config = OrderConfigV4(evaluable(), inputs, outputs, CLAIM_ORDER_NONCE, bytes32(0), "");
        vm.expectCall(address(orderbook), abi.encodeCall(IRaindexV6.addOrder4, (config, new TaskV2[](0))), 1);

        HoloBridgeDeployer deployed = deployBridge();

        assertEq(address(deployed), bridge);
        assertEq(address(deployed.inputToken()), inputToken);
        assertEq(address(deployed.vault()), vault);
    }

    function testAnOrderTheOrderbookAlreadyHoldsRevertsTheWholeDeploy() public {
        orderbookAdds(false);
        vm.expectRevert("HoloBridgeDeployer: the orderbook already holds this claim order");
        deployBridge();
    }

    function testAZeroAdminIsRefused() public {
        orderbookAdds(true);
        vm.expectRevert("HoloBridgeDeployer: zero admin address");
        new HoloBridgeDeployer(
            address(token), address(orderbook), VAULT_ID, MIN_LOCK_AMOUNT, evaluable(), CLAIM_ORDER_NONCE, address(0)
        );
    }
}

/// HoloBridgeDeployer on a fork of Ethereum mainnet, handing the vault to a
/// Safe of three owners that needs two signatures, against the RaindexV6 and
/// Safe 1.4.1 contracts there.
contract TestHoloBridgeDeployerWithSafe is Test, SignContext {
    uint256[] internal ownerKeys;
    address[] internal owners;
    ISafe internal safe;
    HoloBridgeDeployer internal bridge;
    HoloLockVault internal vault;
    bytes internal bytecode;
    address internal signer;
    uint256 internal signerKey;
    address internal locker = makeAddr("locker");
    address internal claimer = makeAddr("claimer");

    function setUp() public {
        vm.createSelectFork(vm.envOr("RPC_URL_ETH", string("https://ethereum-rpc.publicnode.com")));
        for (uint256 i = 0; i < 3; i++) {
            (address owner, uint256 key) = makeAddrAndKey(string.concat("owner ", vm.toString(i)));
            owners.push(owner);
            ownerKeys.push(key);
        }
        safe = ISafe(SAFE_PROXY_FACTORY.createProxyWithNonce(SAFE_SINGLETON, LibSafe.setupCall(owners, 2), 1));
        (signer, signerKey) = makeAddrAndKey("coupon signer");

        bytecode = RAINLANG_DEPLOYER.parse2(composed(signer));
        bridge = new HoloBridgeDeployer(
            address(MAINNET_HOT),
            address(RAINDEX),
            HOLO_VAULT_ID,
            MIN_LOCK_AMOUNT,
            EvaluableV4(RAINLANG_INTERPRETER, RAINLANG_STORE, bytecode),
            CLAIM_ORDER_NONCE,
            address(safe)
        );
        vault = bridge.vault();
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

    function order() internal view returns (OrderV4 memory) {
        IOV2[] memory inputs = new IOV2[](1);
        inputs[0] = IOV2(address(bridge.inputToken()), HOLO_VAULT_ID);
        IOV2[] memory outputs = new IOV2[](1);
        outputs[0] = IOV2(address(MAINNET_HOT), HOLO_VAULT_ID);
        return OrderV4(
            address(vault),
            EvaluableV4(RAINLANG_INTERPRETER, RAINLANG_STORE, bytecode),
            inputs,
            outputs,
            CLAIM_ORDER_NONCE
        );
    }

    /// `data` sent to the vault by the Safe, signed by `keys` of its owners.
    function throughSafe(uint256[] memory signingKeys, bytes memory data) internal returns (bool) {
        bytes memory signatures = LibSafe.sign(vm, signingKeys, LibSafe.transactionHash(safe, address(vault), data));
        return safe.execTransaction(address(vault), 0, data, 0, 0, 0, 0, address(0), payable(address(0)), signatures);
    }

    function keys(uint256 a, uint256 b) internal view returns (uint256[] memory two) {
        two = new uint256[](2);
        (two[0], two[1]) = (ownerKeys[a], ownerKeys[b]);
    }

    function testTheSafeIsTheAdminAndTheClaimOrderIsOnTheOrderbook() public view {
        assertEq(vault.admin(), address(safe));
        assertEq(safe.getThreshold(), 2);
        assertTrue(RAINDEX.orderExists(keccak256(abi.encode(order()))));
    }

    function testAnOwnerAloneCannotAdministerTheVault() public {
        OrderV4 memory claimOrder = order();
        vm.startPrank(owners[0]);
        vm.expectRevert("HoloLockVault: only admin");
        vault.setMinLockAmount(0);
        vm.expectRevert("HoloLockVault: only admin");
        vault.setAdmin(owners[0]);
        vm.expectRevert("HoloLockVault: only admin");
        vault.removeOrder(claimOrder);
        vm.stopPrank();

        vm.startPrank(address(bridge));
        vm.expectRevert("HoloLockVault: only admin");
        vault.setAdmin(address(bridge));
        vm.stopPrank();
    }

    function testTheSafeActsWithTwoOwnerSignaturesAndNotWithOne() public {
        bytes memory call = abi.encodeCall(HoloLockVault.setMinLockAmount, (2e18));
        uint256[] memory one = new uint256[](1);
        one[0] = ownerKeys[0];
        bytes memory signatures = LibSafe.sign(vm, one, LibSafe.transactionHash(safe, address(vault), call));
        vm.expectRevert(bytes("GS020"));
        safe.execTransaction(address(vault), 0, call, 0, 0, 0, 0, address(0), payable(address(0)), signatures);
        assertEq(vault.minLockAmount(), MIN_LOCK_AMOUNT);

        assertTrue(throughSafe(keys(1, 2), call));
        assertEq(vault.minLockAmount(), 2e18);
    }

    function testALockIsClaimedWithACouponAndTheSafeCanRemoveTheOrder() public {
        deal(address(MAINNET_HOT), locker, 100e18);
        vm.startPrank(locker);
        MAINNET_HOT.approve(address(vault), 100e18);
        vault.lock(100e18, keccak256("agent"));
        vm.stopPrank();
        assertEq(vault.vaultBalance(), 100e18);

        OrderV4 memory claimOrder = order();
        bytes32[] memory words = LibCoupon.words(
            claimer,
            40e18,
            block.timestamp + 7 days,
            keccak256(abi.encode(claimOrder)),
            address(vault),
            address(RAINDEX),
            address(MAINNET_HOT),
            HOLO_VAULT_ID,
            keccak256("withdrawal")
        );
        SignedContextV1[] memory signed = new SignedContextV1[](1);
        signed[0] = signContext(signerKey, words);
        TakeOrderConfigV4[] memory orders = new TakeOrderConfigV4[](1);
        orders[0] = TakeOrderConfigV4(claimOrder, 0, 0, signed);
        Float amount = Float.wrap(words[1]);
        vm.prank(claimer);
        RAINDEX.takeOrders4(TakeOrdersConfigV5(amount, amount, CLAIM_IO_RATIO, true, orders, ""));
        assertEq(MAINNET_HOT.balanceOf(claimer), 40e18);
        assertEq(vault.vaultBalance(), 60e18);

        assertTrue(throughSafe(keys(0, 2), abi.encodeCall(HoloLockVault.removeOrder, (claimOrder))));
        assertFalse(RAINDEX.orderExists(keccak256(abi.encode(claimOrder))));
    }
}
