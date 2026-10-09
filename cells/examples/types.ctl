// 🎻 CTL examples, from an inline header to a graph with lazy references.
// These are illustrative types, not the Flame transaction format.

type Hash = [U8; 32];

struct Header {
    version: U16,
    height: U32,
    parent: Hash,
}

struct Blob {
    count: V128,
    data: [U8; count],
}

enum Node: U8 {
    Empty = 0,
    Data { size: V128, data: [U8; size] } = 1,
    Branch { left: ^Node, right: ^Node } = 2,
}

struct Envelope {
    header: Header,
    body: ^Node,
    attachment: ^Cell,
}
