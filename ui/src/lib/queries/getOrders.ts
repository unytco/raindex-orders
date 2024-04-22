import type { Hex } from "viem"

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const getOrders = async (orderHash: Hex, subgraphUrl: string): Promise<any> => {
	const query = `
query MyQuery {
	order (id: "${orderHash}") {
		orderHash
		owner {
		  id
		}
		validInputs {
		  tokenVault {
			vaultId
			token {
			  name
			  symbol
			  decimals
			}
		  }
		}
		validOutputs {
		  tokenVault {
			vaultId
			token {
			  name
			  symbol
			  decimals
			}
			balanceDisplay
		  }
		}
		orderJSONString
		takeOrders {
		  input
		  inputDisplay
		  output
		  outputDisplay
		  inputToken {
			name
		  }
		  outputToken {
			name
		  }
		}
	}
}
`

	const response = await fetch(subgraphUrl, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ query })
	})
	const json = await response.json()

	if (json.errors) {
		console.error(json.errors)
	}

	return json.data
}
