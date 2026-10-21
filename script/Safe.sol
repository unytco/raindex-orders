// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

/// The parts of Safe 1.4.1 the bridge uses. Safe deploys each contract to the
/// same address on every chain. The code hashes below were read on Ethereum
/// mainnet and Base Sepolia, where the code is byte for byte the same.
/// Source: https://github.com/safe-global/safe-smart-account/tree/v1.4.1
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
    function domainSeparator() external view returns (bytes32);
    function getModulesPaginated(address start, uint256 pageSize)
        external
        view
        returns (address[] memory array, address next);
}

interface ISafeProxyFactory {
    function createProxyWithNonce(address singleton, bytes memory initializer, uint256 saltNonce)
        external
        returns (address proxy);
    function proxyCreationCode() external view returns (bytes memory);
}

/// What a Safe's CompatibilityFallbackHandler answers. Called on the Safe itself,
/// `getMessageHash` hashes for that Safe.
interface ICompatibilityFallbackHandler {
    function getMessageHashForSafe(address safe, bytes memory message) external view returns (bytes32);
    function getMessageHash(bytes memory message) external view returns (bytes32);
}

ISafeProxyFactory constant SAFE_PROXY_FACTORY = ISafeProxyFactory(0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67);
// Safe 1.4.1, the singleton for Ethereum mainnet.
address constant SAFE_SINGLETON = 0x41675C099F32341bf84BFc5382aF534df5C7461a;
// SafeL2 1.4.1: Safe with events for each transaction, which Safe's own
// services read on L2s such as Base. Its signature checks are Safe's.
address constant SAFE_L2_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
ICompatibilityFallbackHandler constant SAFE_FALLBACK_HANDLER =
    ICompatibilityFallbackHandler(0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99);

bytes32 constant SAFE_PROXY_FACTORY_CODEHASH = 0x50c3cdc4074750a7a974204a716c999edd37482f907608d960b2b025ee0b3317;
bytes32 constant SAFE_SINGLETON_CODEHASH = 0x1fe2df852ba3299d6534ef416eefa406e56ced995bca886ab7a553e6d0c5e1c4;
bytes32 constant SAFE_L2_SINGLETON_CODEHASH = 0xb1f926978a0f44a2c0ec8fe822418ae969bd8c3f18d61e5103100339894f81ff;
bytes32 constant SAFE_FALLBACK_HANDLER_CODEHASH = 0x7c6007a5d711cea8dfd5d91f5940ec29c7f200fe511eb1fc1397b367af3c42f9;
// SafeProxy 1.4.1, the code of every Safe SAFE_PROXY_FACTORY creates: it
// delegates every call to the singleton in its storage slot 0.
bytes32 constant SAFE_PROXY_CODEHASH = 0xd7d408ebcd99b2b70be43e20253d6d92a8ea8fab29bd3be7f55b10032331fb4c;

// A Safe keeps its singleton in storage slot 0, and its fallback handler at
// keccak256("fallback_manager.handler.address").
bytes32 constant SAFE_SINGLETON_SLOT = bytes32(0);
bytes32 constant SAFE_FALLBACK_HANDLER_SLOT = 0x6c9a6c4a39284e37ed1cf53d337577d14212a4870fb976a4366c693b939918d5;
// Its guard, which checks each of its transactions, at
// keccak256("guard_manager.guard.address"), and the start of its module list.
bytes32 constant SAFE_GUARD_SLOT = 0x4a204f620c8c5ccdca3fd54d003badd85ba500436a431f0cbda4f558c93c34c8;
address constant SAFE_SENTINEL_MODULES = address(0x1);

library LibSafeSetup {
    /// The call that sets a new Safe up: `owners`, `threshold`, the
    /// CompatibilityFallbackHandler, and no module, guard or payment.
    function setupCall(address[] memory owners, uint256 threshold) internal pure returns (bytes memory) {
        return abi.encodeCall(
            ISafe.setup,
            (owners, threshold, address(0), "", address(SAFE_FALLBACK_HANDLER), address(0), 0, payable(address(0)))
        );
    }

    /// Where SAFE_PROXY_FACTORY's createProxyWithNonce puts the Safe of
    /// `singleton`, `initializer` and `saltNonce`: the address commits to all
    /// three, so no other Safe can be created there.
    function predict(address singleton, bytes memory initializer, uint256 saltNonce) internal view returns (address) {
        bytes32 salt = keccak256(abi.encodePacked(keccak256(initializer), saltNonce));
        bytes memory deployment = abi.encodePacked(SAFE_PROXY_FACTORY.proxyCreationCode(), uint256(uint160(singleton)));
        return address(
            uint160(
                uint256(
                    keccak256(abi.encodePacked(bytes1(0xff), address(SAFE_PROXY_FACTORY), salt, keccak256(deployment)))
                )
            )
        );
    }

    /// The SafeMessage hash of `digest` for `safe` on `chainId`: the EIP-712 hash
    /// the owners sign for the Safe's `isValidSignature(digest, signatures)` to
    /// accept, as CompatibilityFallbackHandler's `getMessageHashForSafe(safe,
    /// abi.encode(digest))` computes it.
    function messageHash(uint256 chainId, address safe, bytes32 digest) internal pure returns (bytes32) {
        bytes32 domain =
            keccak256(abi.encode(keccak256("EIP712Domain(uint256 chainId,address verifyingContract)"), chainId, safe));
        bytes32 message = keccak256(abi.encode(keccak256("SafeMessage(bytes message)"), keccak256(abi.encode(digest))));
        return keccak256(abi.encodePacked(bytes1(0x19), bytes1(0x01), domain, message));
    }
}
