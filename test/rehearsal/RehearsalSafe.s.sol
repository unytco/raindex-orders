// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Script, console2} from "forge-std/Script.sol";
import {ISafe, LibSafe, SAFE_PROXY_FACTORY, SAFE_SINGLETON} from "./Safe.sol";

/// Safe transactions for test/fork-rehearsal.sh, sent to its anvil fork only.
contract RehearsalSafe is Script {
    /// Creates a Safe of `owners` that needs 2 of their signatures.
    function create(address[] calldata owners, uint256 salt) external {
        vm.startBroadcast();
        address safe = SAFE_PROXY_FACTORY.createProxyWithNonce(SAFE_SINGLETON, LibSafe.setupCall(owners, 2), salt);
        vm.stopBroadcast();
        console2.log(string.concat("SAFE=", vm.toString(safe)));
    }

    /// Has `safe` call `data` on `to`, signed by the owner keys in
    /// REHEARSAL_OWNER_KEYS.
    function exec(ISafe safe, address to, bytes calldata data) external {
        bytes memory signatures =
            LibSafe.sign(vm, vm.envUint("REHEARSAL_OWNER_KEYS", ","), LibSafe.transactionHash(safe, to, data));
        vm.startBroadcast();
        require(
            safe.execTransaction(to, 0, data, 0, 0, 0, 0, address(0), payable(address(0)), signatures),
            "the Safe transaction failed"
        );
        vm.stopBroadcast();
    }
}
