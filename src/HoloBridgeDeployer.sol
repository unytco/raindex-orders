// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {OrderConfigV4, EvaluableV4, IOV2} from "raindex-interface-0.1.7/src/interface/IRaindexV6.sol";
import {HoloLockVault} from "./HoloLockVault.sol";
import {ClaimInputToken} from "./ClaimInputToken.sol";

/// @title HoloBridgeDeployer
/// @notice Deploys the bridge in one transaction, so that `admin` is the
/// vault's admin from the block the vault exists in.
///
/// It deploys the claim input token, then the vault with itself as the admin,
/// adds the claim order through the vault, and hands the vault to `admin`. All
/// of that is one transaction: this contract is the vault's admin only inside
/// it, and holds no right once it ends. The key that sends the transaction is
/// never the admin. A deploy that fails part way leaves nothing behind, as the
/// transaction reverts as a whole.
contract HoloBridgeDeployer {
    /// @notice The lock vault, whose admin is the `admin` this was deployed with
    HoloLockVault public immutable vault;

    /// @notice The claim order's input token, deployed with the vault
    ClaimInputToken public immutable inputToken;

    /// @param token The ERC20 the vault locks (HOT)
    /// @param orderbook The Raindex V6 address
    /// @param vaultId The vault ID locks deposit into and the claim order pays from
    /// @param minLockAmount The vault's minimum lock
    /// @param evaluable The claim order's interpreter, store and parsed expression
    /// @param orderNonce The claim order's nonce
    /// @param admin The vault's admin from this transaction on: a Safe on mainnet
    constructor(
        address token,
        address orderbook,
        bytes32 vaultId,
        uint256 minLockAmount,
        EvaluableV4 memory evaluable,
        bytes32 orderNonce,
        address admin
    ) {
        require(admin != address(0), "HoloBridgeDeployer: zero admin address");
        ClaimInputToken claimInput = new ClaimInputToken();
        HoloLockVault lockVault = new HoloLockVault(token, orderbook, vaultId, address(this), minLockAmount);

        IOV2[] memory inputs = new IOV2[](1);
        inputs[0] = IOV2(address(claimInput), vaultId);
        IOV2[] memory outputs = new IOV2[](1);
        outputs[0] = IOV2(token, vaultId);
        require(
            lockVault.addOrder(OrderConfigV4(evaluable, inputs, outputs, orderNonce, bytes32(0), "")),
            "HoloBridgeDeployer: the orderbook already holds this claim order"
        );

        lockVault.setAdmin(admin);
        vault = lockVault;
        inputToken = claimInput;
    }
}
