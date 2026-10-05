// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Vm} from "forge-std/Vm.sol";

/// The parts of Safe 1.4.1 the rehearsal uses, at its Ethereum mainnet addresses.
interface ISafe {
    function setup(
        address[] calldata owners,
        uint256 threshold,
        address to,
        bytes calldata data,
        address fallbackHandler,
        address paymentToken,
        uint256 payment,
        address payable paymentReceiver
    ) external;

    function execTransaction(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address payable refundReceiver,
        bytes memory signatures
    ) external payable returns (bool success);

    function getTransactionHash(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address refundReceiver,
        uint256 nonce
    ) external view returns (bytes32);

    function nonce() external view returns (uint256);
    function getOwners() external view returns (address[] memory);
    function getThreshold() external view returns (uint256);
}

interface ISafeProxyFactory {
    function createProxyWithNonce(address singleton, bytes memory initializer, uint256 saltNonce)
        external
        returns (address proxy);
}

interface ICompatibilityFallbackHandler {
    function getMessageHashForSafe(address safe, bytes memory message) external view returns (bytes32);
}

ISafeProxyFactory constant SAFE_PROXY_FACTORY = ISafeProxyFactory(0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67);
address constant SAFE_SINGLETON = 0x41675C099F32341bf84BFc5382aF534df5C7461a;
ICompatibilityFallbackHandler constant SAFE_FALLBACK_HANDLER =
    ICompatibilityFallbackHandler(0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99);

library LibSafe {
    function setupCall(address[] memory owners, uint256 threshold) internal pure returns (bytes memory) {
        return abi.encodeCall(
            ISafe.setup,
            (owners, threshold, address(0), "", address(SAFE_FALLBACK_HANDLER), address(0), 0, payable(address(0)))
        );
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
