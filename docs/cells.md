# Flame Cells

Cell is the data encoding format underpinning all data structures in Flame. Each cell is a data structure that can carry 0 to 8191 bytes of binary data (called "payload") and 0 to 4 references to other cells. Nested cells 

Design of cells in Flame is heavily inspired by cells used within TON blockchain designed by Nikolai Durov, with some important differences:

1. Flame Cells store whole bytes instead of bits.
2. Payload maximum size is considerable larger (8191 bytes vs. 1023 bits).
3. Flame Cells do not store level information or depth.
4. Cells are not first-class types exposed in the FlameVM, but instead used as internal implementation within Strings and Dicts.
5. FlameVM permits transparent loading of pruned branches from externally provided data source. This is used in decoding of Contracts, pruned Actors, Taproot branches and compressed Dicts.
6. Bag-of-Cells (BoC) format is much simpler.

