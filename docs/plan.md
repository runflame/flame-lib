# Flame implementation plan

- cell type language based on TL-B with rust-like syntax. Ignore "linear inversion" operator (~), use byte-oriented syntax. Allow references to earlier-defined variables just like in TL-B. Use built-in types such as u8/u16/u32 (LE) and LEB128. Use "^" for cell references.
- 
