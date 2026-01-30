// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";
import {IOrderBookV3} from "rain.orderbook.interface/interface/IOrderBookV3.sol";

/// @title HoloLockVault
/// @notice A wrapper contract that accepts HOT token deposits and forwards them
/// to a Raindex orderbook vault. Emits events with Holochain agent public keys
/// to enable cross-chain bridging to HoloFuel.
///
/// This contract is the LOCK side of the HOT <> HoloFuel swap:
/// - LOCK: User sends HOT -> receives HoloFuel (this contract)
/// - UNLOCK: User burns HoloFuel -> receives HOT (via claim coupons)
contract HoloLockVault {
    using SafeERC20 for IERC20;

    /// @notice Emitted when HOT is locked for HoloFuel redemption
    /// @param sender The Ethereum address that locked HOT
    /// @param amount The amount of HOT locked (in wei, 18 decimals)
    /// @param holochainAgent The 32-byte Holochain agent public key to receive HoloFuel
    /// @param lockId Unique identifier for this lock operation
    event Lock(
        address indexed sender,
        uint256 amount,
        bytes32 indexed holochainAgent,
        uint256 lockId
    );

    /// @notice Emitted when admin withdraws tokens (emergency only)
    /// @param admin The admin address performing the withdrawal
    /// @param amount The amount withdrawn
    /// @param to The recipient of the withdrawn tokens
    event AdminWithdraw(address indexed admin, uint256 amount, address indexed to);

    /// @notice Emitted when admin is changed
    /// @param oldAdmin The previous admin address
    /// @param newAdmin The new admin address
    event AdminChanged(address indexed oldAdmin, address indexed newAdmin);

    /// @notice The token being locked (HOT on mainnet)
    IERC20 public immutable token;

    /// @notice The Raindex orderbook contract
    IOrderBookV3 public immutable orderbook;

    /// @notice The vault ID used in the orderbook for this contract's deposits
    uint256 public immutable vaultId;

    /// @notice Running counter for unique lock IDs
    uint256 public lockNonce;

    /// @notice Admin address that can perform emergency withdrawals
    address public admin;

    /// @notice Minimum lock amount to prevent dust/spam attacks
    uint256 public minLockAmount;

    /// @dev Restricts function access to admin only
    modifier onlyAdmin() {
        require(msg.sender == admin, "HoloLockVault: only admin");
        _;
    }

    /// @notice Initialize the lock vault
    /// @param _token The ERC20 token to accept (HOT)
    /// @param _orderbook The Raindex orderbook address
    /// @param _vaultId The vault ID to deposit into
    /// @param _admin The admin address for emergency functions
    /// @param _minLockAmount Minimum amount that can be locked
    constructor(
        address _token,
        address _orderbook,
        uint256 _vaultId,
        address _admin,
        uint256 _minLockAmount
    ) {
        require(_token != address(0), "HoloLockVault: zero token address");
        require(_orderbook != address(0), "HoloLockVault: zero orderbook address");
        require(_admin != address(0), "HoloLockVault: zero admin address");

        token = IERC20(_token);
        orderbook = IOrderBookV3(_orderbook);
        vaultId = _vaultId;
        admin = _admin;
        minLockAmount = _minLockAmount;

        // Approve orderbook to spend our tokens for deposits
        // Using max approval since we trust the orderbook contract
        IERC20(_token).safeApprove(_orderbook, type(uint256).max);
    }

    /// @notice Lock tokens to receive HoloFuel on Holochain
    /// @dev Requires prior approval of this contract to spend caller's tokens
    /// @param amount Amount of tokens to lock (must meet minimum)
    /// @param holochainAgent The 32-byte Holochain agent public key to receive HoloFuel
    /// @return lockId The unique identifier for this lock operation
    function lock(uint256 amount, bytes32 holochainAgent) external returns (uint256 lockId) {
        require(amount >= minLockAmount, "HoloLockVault: amount below minimum");
        require(holochainAgent != bytes32(0), "HoloLockVault: invalid holochain agent");

        // Transfer tokens from user to this contract
        token.safeTransferFrom(msg.sender, address(this), amount);

        // Deposit into orderbook vault (this contract owns the vault)
        orderbook.deposit(address(token), vaultId, amount);

        // Generate unique lock ID and emit event
        lockId = lockNonce++;
        emit Lock(msg.sender, amount, holochainAgent, lockId);
    }

    /// @notice Get the current vault balance in the orderbook
    /// @return balance The amount of tokens in the orderbook vault
    function vaultBalance() external view returns (uint256 balance) {
        return orderbook.vaultBalance(address(this), address(token), vaultId);
    }

    /// @notice Admin function for emergency withdrawal from orderbook vault
    /// @dev Only callable by admin, withdraws from orderbook and sends to recipient
    /// @param amount Amount to withdraw
    /// @param to Recipient address
    function adminWithdraw(uint256 amount, address to) external onlyAdmin {
        require(to != address(0), "HoloLockVault: zero recipient");
        require(amount > 0, "HoloLockVault: zero amount");

        // Withdraw from orderbook vault
        orderbook.withdraw(address(token), vaultId, amount);

        // Transfer to recipient
        token.safeTransfer(to, amount);

        emit AdminWithdraw(msg.sender, amount, to);
    }

    /// @notice Admin function to recover any tokens accidentally sent to this contract
    /// @dev Does NOT withdraw from orderbook, only recovers tokens held directly by contract
    /// @param tokenAddress The token to recover
    /// @param to Recipient address
    function adminRecoverTokens(address tokenAddress, address to) external onlyAdmin {
        require(to != address(0), "HoloLockVault: zero recipient");

        uint256 balance = IERC20(tokenAddress).balanceOf(address(this));
        require(balance > 0, "HoloLockVault: no balance");

        IERC20(tokenAddress).safeTransfer(to, balance);
    }

    /// @notice Transfer admin role to a new address
    /// @param newAdmin The new admin address
    function setAdmin(address newAdmin) external onlyAdmin {
        require(newAdmin != address(0), "HoloLockVault: zero admin address");

        address oldAdmin = admin;
        admin = newAdmin;

        emit AdminChanged(oldAdmin, newAdmin);
    }

    /// @notice Update the minimum lock amount
    /// @param _minLockAmount New minimum amount
    function setMinLockAmount(uint256 _minLockAmount) external onlyAdmin {
        minLockAmount = _minLockAmount;
    }
}
