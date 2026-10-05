// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {HoloLockVault} from "src/HoloLockVault.sol";
import {ClaimOrderScript, ClaimNetwork} from "./ClaimOrderScript.sol";

/// @title DeployClaimOrderViaVault
/// @notice Adds the claim order through LOCK_VAULT_ADDRESS on NETWORK, so the
/// vault owns it and claims pay out of the vault locks deposit into. Coupons are
/// accepted from VALID_SIGNER.
contract DeployClaimOrderViaVault is ClaimOrderScript {
    function run() external {
        ClaimNetwork memory net = networkFromEnv();
        HoloLockVault vault = HoloLockVault(vm.envAddress("LOCK_VAULT_ADDRESS"));
        address signer = vm.envAddress("VALID_SIGNER");
        requireSigner(net, signer);
        require(
            address(vault.orderbook()) == address(net.orderbook), "the vault's orderbook is not NETWORK's OrderBookV3"
        );

        bytes[] memory calls = new bytes[](1);
        calls[0] = abi.encodeCall(HoloLockVault.addOrder, (claimOrderConfig(net, address(vault.token()), signer)));
        asAdmin(vault, calls);
    }
}
