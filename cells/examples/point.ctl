// Wire records; the application converts Point into a validated domain type.
type Point = [U8; 32];

struct Key {
    point: Point,
}

struct Points {
    count: U8,
    items: [Point; count],
    commitment: ^Point,
}
