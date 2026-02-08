// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {MockERC20} from "forge-std/mocks/MockERC20.sol";

/// @title MockHOT
/// @notice A mock HOT token for testing the HoloLockVault
/// @dev Exposes mint function for test purposes
contract MockHOT is MockERC20 {
    constructor() {
        initialize("Mock HOT Token", "HOT", 18);
    }

    /// @notice Mint tokens to an address (for testing only)
    /// @param to The recipient address
    /// @param amount The amount to mint
    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    /// @notice Burn tokens from an address (for testing only)
    /// @param from The address to burn from
    /// @param amount The amount to burn
    function burn(address from, uint256 amount) external {
        _burn(from, amount);
    }
}
