// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {Test} from "forge-std/Test.sol";
import {IRaindexV6, OrderConfigV4} from "raindex-interface-0.1.7/src/interface/IRaindexV6.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {MockHOT} from "src/test/MockHOT.sol";
import {MockRaindex} from "src/test/MockRaindex.sol";
import {ClaimOrderScript, AdminInputs} from "script/ClaimOrderScript.sol";

/// A Safe as the admin checks read it: a threshold and owners.
contract MockSafe {
    uint256 public immutable threshold;
    address[] internal owners;

    constructor(uint256 _threshold, address[] memory _owners) {
        threshold = _threshold;
        owners = _owners;
    }

    function getThreshold() external view returns (uint256) {
        return threshold;
    }

    function getOwners() external view returns (address[] memory) {
        return owners;
    }
}

contract AdminHarness is ClaimOrderScript {
    function acceptAdmin(address admin, AdminInputs memory given) external view {
        requireAdmin(admin, given);
    }

    function contractAt(address account) external view returns (bool) {
        return isContract(account);
    }

    function sendAsAdmin(HoloLockVault vault, bytes[] memory calls) external {
        asAdmin(vault, calls);
    }

    function acceptSigner(address signer) external view {
        requireSigner(claimNetwork("mainnet"), signer);
    }
}

contract TestClaimOrderScript is Test {
    AdminHarness internal harness = new AdminHarness();
    address internal constant ACCOUNT = address(0xA11CE);

    function hasContractCode(bytes memory runtime) internal returns (bool) {
        vm.etch(ACCOUNT, runtime);
        return harness.contractAt(ACCOUNT);
    }

    function testAKeyWithOrWithoutAnEip7702DelegationIsAKey() public {
        assertFalse(hasContractCode(""));
        assertFalse(hasContractCode(abi.encodePacked(hex"ef0100", address(0xBEEF))));
    }

    function testAnyOtherCodeIsAContract() public {
        assertTrue(hasContractCode(abi.encodePacked(hex"600100", address(0xBEEF))));
        assertTrue(hasContractCode(address(harness).code));
    }

    /// Raindex V6 asks any signer with code through EIP-1271, a delegated key's
    /// delegate included, so such a key cannot sign coupons.
    function testAKeyWithAnEip7702DelegationIsRefusedAsTheSigner() public {
        harness.acceptSigner(ACCOUNT);

        vm.etch(ACCOUNT, abi.encodePacked(hex"ef0100", address(0xBEEF)));
        vm.expectRevert(
            bytes(
                "VALID_SIGNER has an EIP-7702 delegation, which Raindex V6 asks for a coupon's signature in place of the key: use a key with no delegation, or a Safe"
            )
        );
        harness.acceptSigner(ACCOUNT);
    }

    /// A vault whose admin is the broadcaster a test sends as, with `adminCode` at it.
    function vaultAdministeredBy(bytes memory adminCode) internal returns (HoloLockVault vault, bytes[] memory calls) {
        MockRaindex orderbook = new MockRaindex();
        vault = new HoloLockVault(address(new MockHOT()), address(orderbook), bytes32(uint256(1)), DEFAULT_SENDER, 1);
        vm.etch(DEFAULT_SENDER, adminCode);
        vm.mockCall(address(orderbook), abi.encodeWithSelector(IRaindexV6.addOrder4.selector), abi.encode(true));
        OrderConfigV4 memory config;
        calls = new bytes[](1);
        calls[0] = abi.encodeCall(HoloLockVault.addOrder, (config));
    }

    address[] internal owners;

    function ownersOf(uint256 count) internal returns (address[] memory) {
        delete owners;
        for (uint256 i = 0; i < count; i++) {
            owners.push(makeAddr(string.concat("owner ", vm.toString(i))));
        }
        return owners;
    }

    function inputs(uint256 threshold, address[] memory stated) internal pure returns (AdminInputs memory) {
        return AdminInputs(false, threshold, stated);
    }

    function singleKeyAllowed(AdminInputs memory given) internal pure returns (AdminInputs memory) {
        given.singleKeyAllowed = true;
        return given;
    }

    function noOwners() internal pure returns (address[] memory) {
        return new address[](0);
    }

    function testAKeyAsTheAdminIsRefusedUnlessOneKeyIsAllowed() public {
        vm.expectRevert(
            bytes(
                "ADMIN_ADDRESS 0x00000000000000000000000000000000000A11cE is a key, not a Safe: on mainnet the vault admin is a Safe from the first transaction (docs/enable-multisig.md). To hand the vault to one key regardless, set UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes"
            )
        );
        harness.acceptAdmin(ACCOUNT, inputs(0, noOwners()));

        harness.acceptAdmin(ACCOUNT, singleKeyAllowed(inputs(0, noOwners())));
        // A delegated key is a key.
        vm.etch(ACCOUNT, abi.encodePacked(hex"ef0100", address(0xBEEF)));
        harness.acceptAdmin(ACCOUNT, singleKeyAllowed(inputs(0, noOwners())));
    }

    function testAContractThatIsNotASafeIsRefused() public {
        vm.expectRevert(
            bytes(string.concat("ADMIN_ADDRESS ", vm.toString(address(harness)), " is a contract but not a Safe"))
        );
        harness.acceptAdmin(address(harness), singleKeyAllowed(inputs(2, ownersOf(3))));
    }

    /// The refusal a contract answering as a Safe gets once its threshold and
    /// owners pass: it is not Safe's own proxy, so something other than its
    /// owners may act as it. test/TestOneSafeAdminAndSigner.t.sol takes a real
    /// Safe 1.4.1 on a fork of mainnet.
    function notSafesProxy(address safe) internal pure returns (bytes memory) {
        return bytes(
            string.concat(
                "ADMIN_ADDRESS ",
                vm.toString(safe),
                " is not a Safe 1.4.1 that only its owners act as: its code is not Safe's SafeProxy 1.4.1 (docs/enable-multisig.md)"
            )
        );
    }

    function testAContractAnsweringAsTheStatedSafeIsRefusedUnlessItIsSafesProxy() public {
        MockSafe safe = new MockSafe(2, ownersOf(3));
        address[] memory reversed = new address[](3);
        (reversed[0], reversed[1], reversed[2]) = (owners[2], owners[0], owners[1]);

        vm.expectRevert(notSafesProxy(address(safe)));
        harness.acceptAdmin(address(safe), inputs(2, owners));
        vm.expectRevert(notSafesProxy(address(safe)));
        harness.acceptAdmin(address(safe), inputs(2, reversed));
    }

    function testASafeOfThresholdOneIsRefusedUnlessOneKeyIsAllowed() public {
        MockSafe safe = new MockSafe(1, ownersOf(3));

        vm.expectRevert(
            bytes(
                string.concat(
                    "ADMIN_ADDRESS ",
                    vm.toString(address(safe)),
                    " is a Safe of threshold 1 with 3 owners, which one key controls. Use a Safe of threshold 2 or more, or set UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes"
                )
            )
        );
        harness.acceptAdmin(address(safe), inputs(1, owners));

        vm.expectRevert(notSafesProxy(address(safe)));
        harness.acceptAdmin(address(safe), singleKeyAllowed(inputs(1, owners)));
    }

    function testTheStatedThresholdAndOwnersMustMatchTheSafe() public {
        MockSafe safe = new MockSafe(2, ownersOf(3));
        string memory at = vm.toString(address(safe));

        vm.expectRevert(bytes("ADMIN_SAFE_THRESHOLD is required: the threshold ADMIN_ADDRESS is expected to have"));
        harness.acceptAdmin(address(safe), inputs(0, owners));

        vm.expectRevert(
            bytes(
                string.concat(
                    "ADMIN_ADDRESS ",
                    at,
                    " is a Safe of threshold 2 with 3 owners, not ADMIN_SAFE_THRESHOLD=3: is it the Safe meant?"
                )
            )
        );
        harness.acceptAdmin(address(safe), inputs(3, owners));

        vm.expectRevert(
            bytes("ADMIN_SAFE_OWNERS is required: the owners ADMIN_ADDRESS is expected to have, separated by commas")
        );
        harness.acceptAdmin(address(safe), inputs(2, noOwners()));

        string memory listed = string.concat(
            "ADMIN_SAFE_OWNERS does not list the owners of ADMIN_ADDRESS ",
            at,
            ", which are ",
            vm.toString(owners[0]),
            ", ",
            vm.toString(owners[1]),
            ", ",
            vm.toString(owners[2]),
            ": is it the Safe meant?"
        );
        address[] memory two = new address[](2);
        (two[0], two[1]) = (owners[0], owners[1]);
        vm.expectRevert(bytes(listed));
        harness.acceptAdmin(address(safe), inputs(2, two));

        address[] memory twice = new address[](3);
        (twice[0], twice[1], twice[2]) = (owners[0], owners[1], owners[1]);
        vm.expectRevert(bytes(listed));
        harness.acceptAdmin(address(safe), inputs(2, twice));

        address[] memory other = new address[](3);
        (other[0], other[1], other[2]) = (owners[0], owners[1], makeAddr("not an owner"));
        vm.expectRevert(bytes(listed));
        harness.acceptAdmin(address(safe), inputs(2, other));
    }

    function testADelegatedAdminKeySendsItsCalls() public {
        (HoloLockVault vault, bytes[] memory calls) =
            vaultAdministeredBy(abi.encodePacked(hex"ef0100", address(0xBEEF)));
        vm.expectCall(address(vault), calls[0], 1);
        harness.sendAsAdmin(vault, calls);
    }

    function testAContractAdminSendsNothing() public {
        (HoloLockVault vault, bytes[] memory calls) = vaultAdministeredBy(address(harness).code);
        vm.expectCall(address(vault), calls[0], 0);
        harness.sendAsAdmin(vault, calls);
    }
}
