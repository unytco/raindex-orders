# Coupon Signer (coupon-signer/)
A Rust CLI tool that generates SignedContext coupons compatible with the Raindex holo-claim.rain order.

## Key Handling

The private key is stored in the .env file as SIGNER_PRIVATE_KEY. The corresponding public address must be configured in the Rainlang order as valid-signer (currently set to 0x8E72b7568738da52ca3DCd9b24E178127A4E7d37 in holo-claim.rain).

##Usage

```
cp .env.example .env  # Uses test key by default

cargo run -- \
    --amount "10" \
    --recipient "0x..." \
    --order-hash "0x..." \
    --order-owner "0x..." \
    --orderbook "0xfca89cD12Ba1346b1ac570ed988AB43b812733fe" \
    --token "0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b" \
    --vault-id "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b"
```
  
## Output

  The tool outputs a JSON containing:
  - Human-readable fields (recipient, amount, expiry, nonce)
  - signed_context with signer address, context array (9 uint256 values), and signature

  The signed_context can be passed directly to Raindex's takeOrders function.

  Context Array Structure (matches holo-claim.rain)

  - [0] recipient address
  - [1] amount (wei)
  - [2] expiry timestamp
  - [3] order hash
  - [4] order owner
  - [5] orderbook address
  - [6] token address
  - [7] vault ID
  - [8] nonce
