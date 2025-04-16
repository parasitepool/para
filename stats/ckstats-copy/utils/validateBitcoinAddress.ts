const bitcoin = require('bitcoinjs-lib');

export function validateBitcoinAddress(address: string): boolean {
  // Basic regex for Bitcoin addresses
  // const regex = /^[13][a-km-zA-HJ-NP-Z1-9]{25,34}$|^bc1[ac-hj-np-z02-9]{39,59}$/;
  // return regex.test(address);
  try {
    // Check for legacy addresses (P2PKH and P2SH) and SegWit addresses (P2WPKH and P2WSH)
    if (address.startsWith('1') || address.startsWith('3') || address.startsWith('bc1q')) {
      bitcoin.address.toOutputScript(address);
      return true;
    }
    
    // Check for Taproot addresses (P2TR)
    if (address.startsWith('bc1p')) {
      // Taproot addresses are 62 characters long
      if (address.length !== 62) {
        throw new Error('Invalid Taproot address length');
      }
      return true;
    }
    
    // If none of the above conditions are met, it's not a valid Bitcoin address
    throw new Error('Invalid Bitcoin address format');
  } catch (e) {
    console.log(`Invalid Bitcoin address: ${address}`);
    return false;
  }
}