// SPDX-License-Identifier: CAL
pragma solidity =0.8.25;

import {HoloBridgeDeployer} from "src/HoloBridgeDeployer.sol";
import {MAINNET_HOT} from "src/Constants.sol";
import {DeployBridge} from "./DeployBridge.s.sol";

/// Deploys the HOT bridge on Ethereum mainnet against the RaindexV6 Rain
/// deployed there, in one transaction, with a Safe as the vault admin from the
/// start (DeployBridge). deploy-mainnet.sh runs it, then `record`.
contract DeployMainnet is DeployBridge {
    function run() external {
        deploy(claimNetwork("mainnet"), address(MAINNET_HOT));
    }

    /// Prints the deploy record of the vault `bridge` deployed, and refuses a
    /// vault whose admin was ever anything but `bridge` and then ADMIN_ADDRESS.
    function record(HoloBridgeDeployer bridge, uint256 fromBlock) external {
        recordOf(claimNetwork("mainnet"), address(MAINNET_HOT), bridge, fromBlock);
    }
}
