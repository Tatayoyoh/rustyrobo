// Invented data of the demo video (dev/demo-video.sh), run by mongosh.
const shop = db.getSiblingDB('shop');
const names = ['Ada Moreau', 'Linus Berg', 'Grace Okafor', 'Alan Duarte', 'Hedy Novak', 'Edsger Laine', 'Barbara Ito', 'Ken Alvarez'];
const cities = ['Lyon', 'Porto', 'Gdańsk', 'Ghent', 'Turin', 'Malmö', 'Leeds', 'Brno'];
const products = [
  { sku: 'MUG-01', name: 'Enamel mug', price: 12.5 },
  { sku: 'LMP-02', name: 'Desk lamp', price: 39.9 },
  { sku: 'NTB-03', name: 'Dot grid notebook', price: 8.0 },
  { sku: 'PEN-04', name: 'Fountain pen', price: 24.0 },
  { sku: 'BAG-05', name: 'Canvas tote', price: 15.0 },
];
shop.products.insertMany(products);
shop.customers.insertMany(names.map((name, i) => ({ name, email: name.toLowerCase().replace(' ', '.') + '@example.com', city: cities[i] })));
const orders = [];
for (let i = 0; i < 40; i++) {
  const items = [0, 1, 2].slice(0, 1 + (i % 3)).map((k) => {
    const p = products[(i + k * 2) % products.length];
    return { sku: p.sku, name: p.name, qty: 1 + ((i + k) % 3), price: p.price };
  });
  orders.push({
    number: 'A-' + (1001 + i),
    status: ['paid', 'shipped', 'delivered', 'pending'][i % 4],
    customer: { name: names[i % names.length], city: cities[i % cities.length] },
    items,
    total: Math.round(items.reduce((t, it) => t + it.qty * it.price, 0) * 100) / 100,
    createdAt: new Date(Date.UTC(2026, 9, 1 + (i % 28), 9 + (i % 9), (i * 7) % 60)),
  });
}
shop.orders.insertMany(orders);
shop.orders.createIndex({ number: 1 }, { unique: true });
db.getSiblingDB('blog').posts.insertMany([
  { title: 'Hello', tags: ['news'], published: true },
  { title: 'Release notes', tags: ['release'], published: false },
]);
