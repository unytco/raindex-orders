// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {Vm} from "forge-std/Vm.sol";
import {
    ISafe,
    ISafeProxyFactory,
    ICompatibilityFallbackHandler,
    LibSafeSetup,
    SAFE_PROXY_FACTORY,
    SAFE_SINGLETON,
    SAFE_FALLBACK_HANDLER
} from "script/Safe.sol";

/// Owner signatures for the rehearsal and the fork tests. The Safe interfaces,
/// addresses and setup are script/Safe.sol's.
library LibSafe {
    function setupCall(address[] memory owners, uint256 threshold) internal pure returns (bytes memory) {
        return LibSafeSetup.setupCall(owners, threshold);
    }

    /// The hash the owners sign for `safe` to call `data` on `to`, at its next nonce.
    function transactionHash(ISafe safe, address to, bytes memory data) internal view returns (bytes32) {
        return safe.getTransactionHash(to, 0, data, 0, 0, 0, 0, address(0), address(0), safe.nonce());
    }

    /// What a coupon carries for `safe` as its signer: owner signatures over the
    /// SafeMessage of `digest`, which `isValidSignature(digest, signatures)` checks.
    function couponSignature(Vm vm, address safe, uint256[] memory ownerKeys, bytes32 digest)
        internal
        view
        returns (bytes memory)
    {
        return sign(vm, ownerKeys, SAFE_FALLBACK_HANDLER.getMessageHashForSafe(safe, abi.encode(digest)));
    }

    /// The SafeMessage hash of `digest` for `safe` on `chainId`, computed as
    /// Safe 1.3.0 and later do, and as bridge-orchestrator/src/signer.rs does
    /// offline: `getMessageHashForSafe(safe, abi.encode(digest))`.
    function messageHash(uint256 chainId, address safe, bytes32 digest) internal pure returns (bytes32) {
        return LibSafeSetup.messageHash(chainId, safe, digest);
    }

    /// Each owner's 65-byte signature of `hash`, in ascending owner order as Safe
    /// requires.
    function sign(Vm vm, uint256[] memory ownerKeys, bytes32 hash) internal pure returns (bytes memory signatures) {
        uint256[] memory keys = new uint256[](ownerKeys.length);
        for (uint256 i = 0; i < keys.length; i++) {
            keys[i] = ownerKeys[i];
            for (uint256 j = i; j > 0 && vm.addr(keys[j - 1]) > vm.addr(keys[j]); j--) {
                (keys[j - 1], keys[j]) = (keys[j], keys[j - 1]);
            }
        }
        for (uint256 i = 0; i < keys.length; i++) {
            (uint8 v, bytes32 r, bytes32 s) = vm.sign(keys[i], hash);
            signatures = abi.encodePacked(signatures, r, s, v);
        }
    }
}
