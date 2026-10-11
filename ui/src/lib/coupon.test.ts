import { describe, expect, it } from 'vitest'
import { deserializeSignedContext, parseCoupon } from './coupon'

const word = (hex: string) => BigInt(hex).toString()

describe('parseCoupon', () => {
	it('keeps the leading zeros of addresses and hashes', () => {
		const coupon = parseCoupon(
			deserializeSignedContext(
				[
					'0x8E72b7568738da52ca3DCd9b24E178127A4E7d37',
					'0x00',
					word('0x00000000000000000000000000000000000000aa'),
					'1000000000000000000',
					'1776472741',
					word('0x0eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9'),
					word('0x00E064e3C2EEf66cb93dA8D8114F5084E92F48D6'),
					word('0x0fa89cD12Ba1346b1ac570ed988AB43b812733fe'),
					word('0x0aC8eEEE9f84F3E3F592e9D8604100eA1b788749'),
					word('0x0ede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b'),
					'7'
				].join(',')
			)
		)

		expect(coupon.recipient).toBe('0x00000000000000000000000000000000000000aa')
		expect(coupon.orderHash).toBe(
			'0x0eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9'
		)
		expect(coupon.orderOwner).toBe('0x00e064e3c2eef66cb93da8d8114f5084e92f48d6')
		expect(coupon.orderbookAddress).toBe('0x0fa89cd12ba1346b1ac570ed988ab43b812733fe')
		expect(coupon.claimTokenAddress).toBe('0x0ac8eeee9f84f3e3f592e9d8604100ea1b788749')
		expect(coupon.outputVaultId).toBe(
			'0x0ede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b'
		)
	})

	it.each([
		['a signer one character short', '0x8E72b7568738da52ca3DCd9b24E178127A4E7d3'],
		['a signature that is not hex', '0x8E72b7568738da52ca3DCd9b24E178127A4E7d37', '0xzz']
	])('refuses %s, so the page calls it malformed', (_case, signer, signature = '0x00') => {
		expect(() => deserializeSignedContext([signer, signature, '1', '2'].join(','))).toThrow(
			'Not a coupon'
		)
	})

	it('takes a signer in any letter case, as the orderbook does', () => {
		const { signer } = deserializeSignedContext(
			'0x8e72b7568738da52ca3DCd9b24E178127A4E7d37,0x00,1,2'
		)

		expect(signer).toBe('0x8e72b7568738da52ca3dcd9b24e178127a4e7d37')
	})
})
