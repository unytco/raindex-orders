import moment from 'moment'

export function formatDate(timestamp: string): string {
	return moment(parseInt(timestamp) * 1000).fromNow()
}

export function truncateEthAddress(address: string, length: number = 3): string {
	if (!address.startsWith('0x') || address.length !== 42) {
		throw new Error('Invalid Ethereum address')
	}

	if (address.length <= 2 * length + 2) {
		return address
	}

	return `${address.substring(0, length + 2)}...${address.substring(address.length - length)}`
}