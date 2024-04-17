import moment from 'moment'

export function formatDate(timestamp: string): string {
	return moment(parseInt(timestamp) * 1000).fromNow()
}
