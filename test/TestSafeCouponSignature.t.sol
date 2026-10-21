// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {Test} from "forge-std/Test.sol";
import {ISafe, LibSafe, SAFE_PROXY_FACTORY, SAFE_SINGLETON, SAFE_FALLBACK_HANDLER} from "./rehearsal/Safe.sol";

/// The hash a Safe's owners sign for a coupon, pinned against the Safe 1.4.1
/// contracts on a fork of Ethereum mainnet. bridge-orchestrator/src/signer.rs
/// computes the same hash offline and reproduces this vector.
contract TestSafeCouponSignature is Test {
    bytes4 private constant EIP1271_MAGIC_VALUE = 0x1626ba7e;

    /// The coupon test vector's digest: test/TestCouponSignature.t.sol.
    bytes32 internal constant DIGEST = 0xf32a9e86bb3d75c00acec45f208001679a5f2a7976c5ce3327b6de7cecb0781f;

    /// Anvil's accounts 1, 2 and 3 as the owners of a Safe of threshold 2.
    uint256[] internal ownerKeys = [
        0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d,
        0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a,
        0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6
    ];
    ISafe internal safe;

    function setUp() public {
        vm.createSelectFork(vm.envOr("RPC_URL_ETH", string("https://ethereum-rpc.publicnode.com")));
        address[] memory owners = new address[](3);
        for (uint256 i = 0; i < 3; i++) {
            owners[i] = vm.addr(ownerKeys[i]);
        }
        safe = ISafe(SAFE_PROXY_FACTORY.createProxyWithNonce(SAFE_SINGLETON, LibSafe.setupCall(owners, 2), 7));
    }

    /// The Safe at this deterministic address, and the hash its handler has the
    /// owners sign, are the vector `safe_message_hash` reproduces in Rust.
    function testTheSafeMessageHashIsComputedAsTheFallbackHandlerComputesIt() public view {
        assertEq(address(safe), 0x3838Fc4E0cba211c3D6D202539bB2dc2B72C7a22);
        bytes32 onChain = SAFE_FALLBACK_HANDLER.getMessageHashForSafe(address(safe), abi.encode(DIGEST));
        assertEq(onChain, 0xd02d17b3c4226662353fa285586b495d7fcf937967ac510d2ebf535047b68d34);
        assertEq(LibSafe.messageHash(1, address(safe), DIGEST), onChain);
    }

    /// Two owners' signatures of that hash, in ascending owner order, are what
    /// the Safe accepts as the coupon's signature; one, a non-owner's, or an
    /// owner's twice are not.
    function testTheSafeAcceptsTwoOwnerSignaturesOfThatHashAndNothingLess() public {
        bytes32 hash = LibSafe.messageHash(1, address(safe), DIGEST);
        uint256[] memory two = new uint256[](2);
        (two[0], two[1]) = (ownerKeys[2], ownerKeys[0]);
        assertEq(safeIsValid(LibSafe.sign(vm, two, hash)), EIP1271_MAGIC_VALUE);

        uint256[] memory one = new uint256[](1);
        one[0] = ownerKeys[0];
        assertReverts(LibSafe.sign(vm, one, hash), "GS020");

        (, uint256 stranger) = makeAddrAndKey("not an owner");
        (two[0], two[1]) = (ownerKeys[0], stranger);
        assertReverts(LibSafe.sign(vm, two, hash), "GS026");

        (two[0], two[1]) = (ownerKeys[1], ownerKeys[1]);
        assertReverts(LibSafe.sign(vm, two, hash), "GS026");
    }

    function safeIsValid(bytes memory signature) internal view returns (bytes4) {
        (bool ok, bytes memory answer) =
            address(safe).staticcall(abi.encodeWithSignature("isValidSignature(bytes32,bytes)", DIGEST, signature));
        require(ok, "isValidSignature reverted");
        return abi.decode(answer, (bytes4));
    }

    function assertReverts(bytes memory signature, string memory reason) internal view {
        (bool ok, bytes memory answer) =
            address(safe).staticcall(abi.encodeWithSignature("isValidSignature(bytes32,bytes)", DIGEST, signature));
        assertFalse(ok);
        assertEq(abi.decode(slice(answer), (string)), reason);
    }

    function slice(bytes memory revertData) internal pure returns (bytes memory payload) {
        payload = new bytes(revertData.length - 4);
        for (uint256 i = 0; i < payload.length; i++) {
            payload[i] = revertData[i + 4];
        }
    }
}
