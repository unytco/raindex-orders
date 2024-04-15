// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Vm} from "forge-std/Vm.sol";
import {Strings} from "openzeppelin-contracts/contracts/utils/Strings.sol";

library LibComposeOrders {
    using Strings for address;
    using Strings for uint256;

    function getComposedOrder(
        Vm vm,
        string memory filePath,
        string memory scenario,
        string memory buildPath,
        string memory manifestPath
    ) internal returns (bytes memory rainlang) {
        string[] memory ffi = new string[](16);
        ffi[0] = "nix";
        ffi[1] = "develop";
        ffi[2] = buildPath;
        ffi[3] = "--command";
        ffi[4] = "cargo";
        ffi[5] = "run";
        ffi[6] = "--manifest-path";
        ffi[7] = manifestPath;
        ffi[8] = "--package";
        ffi[9] = "rain_orderbook_cli";
        ffi[10] = "order";
        ffi[11] = "compose";
        ffi[12] = "-f";
        ffi[13] = filePath;
        ffi[14] = "-s";
        ffi[15] = scenario;

        rainlang = vm.ffi(ffi);
    }
}
