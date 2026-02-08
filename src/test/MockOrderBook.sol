// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";
import {
    IOrderBookV3,
    OrderConfigV2,
    OrderV2,
    ClearConfig,
    SignedContextV1,
    IERC3156FlashBorrower,
    TakeOrdersConfigV2
} from "rain.orderbook.interface/interface/IOrderBookV3.sol";

/// @title MockOrderBook
/// @notice A minimal mock orderbook for testing HoloLockVault
/// @dev Only implements deposit, withdraw, and vaultBalance
contract MockOrderBook is IOrderBookV3 {
    using SafeERC20 for IERC20;

    /// @dev Mapping: owner -> token -> vaultId -> balance
    mapping(address => mapping(address => mapping(uint256 => uint256))) private _vaultBalances;

    /// @inheritdoc IOrderBookV3
    function deposit(address token, uint256 vaultId, uint256 amount) external override {
        require(amount > 0, "ZeroDepositAmount");

        // Transfer tokens from sender to this contract
        IERC20(token).safeTransferFrom(msg.sender, address(this), amount);

        // Update vault balance
        _vaultBalances[msg.sender][token][vaultId] += amount;

        emit Deposit(msg.sender, token, vaultId, amount);
    }

    /// @inheritdoc IOrderBookV3
    function withdraw(address token, uint256 vaultId, uint256 targetAmount) external override {
        require(targetAmount > 0, "ZeroWithdrawTargetAmount");

        uint256 currentBalance = _vaultBalances[msg.sender][token][vaultId];
        uint256 actualAmount = targetAmount > currentBalance ? currentBalance : targetAmount;

        if (actualAmount > 0) {
            _vaultBalances[msg.sender][token][vaultId] -= actualAmount;
            IERC20(token).safeTransfer(msg.sender, actualAmount);
        }

        emit Withdraw(msg.sender, token, vaultId, targetAmount, actualAmount);
    }

    /// @inheritdoc IOrderBookV3
    function vaultBalance(address owner, address token, uint256 id) external view override returns (uint256) {
        return _vaultBalances[owner][token][id];
    }

    // Stub implementations for other functions (not used by HoloLockVault)

    /// @inheritdoc IOrderBookV3
    function addOrder(OrderConfigV2 calldata) external pure override returns (bool) {
        revert("Not implemented");
    }

    /// @inheritdoc IOrderBookV3
    function orderExists(bytes32) external pure override returns (bool) {
        revert("Not implemented");
    }

    /// @inheritdoc IOrderBookV3
    function removeOrder(OrderV2 calldata) external pure override returns (bool) {
        revert("Not implemented");
    }

    /// @inheritdoc IOrderBookV3
    function clear(
        OrderV2 memory,
        OrderV2 memory,
        ClearConfig calldata,
        SignedContextV1[] memory,
        SignedContextV1[] memory
    ) external pure override {
        revert("Not implemented");
    }

    /// @inheritdoc IOrderBookV3
    function takeOrders(TakeOrdersConfigV2 calldata) external pure override returns (uint256, uint256) {
        revert("Not implemented");
    }

    function flashLoan(IERC3156FlashBorrower, address, uint256, bytes calldata) external pure override returns (bool) {
        revert("Not implemented");
    }

    function flashFee(address, uint256) external pure override returns (uint256) {
        revert("Not implemented");
    }

    function maxFlashLoan(address) external pure override returns (uint256) {
        revert("Not implemented");
    }
}
