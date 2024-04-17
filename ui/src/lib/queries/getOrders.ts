// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const getOrders = async (subgraphUrl: string): Promise<any> => {
	const query = `
  query MyQuery {
  orders {
    id
    orderJSONString
  }
}
    `;

	const response = await fetch(subgraphUrl, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ query })
	});
	const json = await response.json();
	if (json.errors) {
		console.error(json.errors);
	}

	return json.data;
};
