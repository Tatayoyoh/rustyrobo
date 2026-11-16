// Invented data of the screenshots (dev/screenshots.sh), run by mongosh
// before the data of the demo video (shop, blog).
['rrtest', 'shop', 'blog'].forEach((name) => db.getSiblingDB(name).dropDatabase());
const items = [];
for (let n = 0; n < 120; n++) {
  const item = { n, tag: n % 15 === 0 ? 'fizzbuzz' : n % 3 === 0 ? 'fizz' : n % 5 === 0 ? 'buzz' : String(n), at: new Date(Date.UTC(2024, 0, 1 + n)), l: NumberLong(n * 1000) };
  if (n % 2) item.price = NumberDecimal((n * 1.25).toFixed(2));
  items.push(item);
}
db.getSiblingDB('rrtest').items.insertMany(items);
