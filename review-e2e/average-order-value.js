export function averageCompletedOrderValue(orders) {
  const completedOrders = orders.filter((order) => order.status !== "cancelled");
  const total = completedOrders.reduce((sum, order) => sum + order.total, 0);

  return total / orders.length;
}
