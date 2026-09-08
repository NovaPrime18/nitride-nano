// Faithful port of lz-string 1.4.4 decompressFromBase64
const keyStrBase64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
const baseReverseDic = {};
function getBaseValue(alphabet, character) {
  if (!baseReverseDic[alphabet]) baseReverseDic[alphabet] = {};
  let dic = baseReverseDic[alphabet];
  if (!(character in dic)) dic[character] = alphabet.indexOf(character);
  return dic[character];
}
function decompressFromBase64(input) {
  if (!input) return "";
  return _decompress(input.length, 32, (index) => getBaseValue(keyStrBase64, input.charAt(index)));
}
function _decompress(length, resetValue, getNextValue) {
  const dictionary = [];
  let next, enlargeIn = 4, dictSize = 4, numBits = 3, entry = "", result = "", i, w, bits, resb, maxpower, power, c;
  const data = { val: getNextValue(0), position: resetValue, index: 1 };
  for (i = 0; i < 3; i += 1) dictionary[i] = i;

  bits = 0; maxpower = Math.pow(2, 2); power = 1;
  while (power !== maxpower) {
    resb = data.val & data.position;
    data.position >>= 1;
    if (data.position === 0) { data.position = resetValue; data.val = getNextValue(data.index++); }
    bits |= (resb > 0 ? 1 : 0) * power;
    power <<= 1;
  }
  next = bits;
  switch (next) {
    case 0:
      bits = 0; maxpower = Math.pow(2, 8); power = 1;
      while (power !== maxpower) {
        resb = data.val & data.position; data.position >>= 1;
        if (data.position === 0) { data.position = resetValue; data.val = getNextValue(data.index++); }
        bits |= (resb > 0 ? 1 : 0) * power; power <<= 1;
      }
      c = String.fromCharCode(bits); break;
    case 1:
      bits = 0; maxpower = Math.pow(2, 16); power = 1;
      while (power !== maxpower) {
        resb = data.val & data.position; data.position >>= 1;
        if (data.position === 0) { data.position = resetValue; data.val = getNextValue(data.index++); }
        bits |= (resb > 0 ? 1 : 0) * power; power <<= 1;
      }
      c = String.fromCharCode(bits); break;
    case 2: return "";
  }
  dictionary[3] = c;
  w = result = c;
  while (true) {
    if (data.index > length) return "";
    bits = 0; maxpower = Math.pow(2, numBits); power = 1;
    while (power !== maxpower) {
      resb = data.val & data.position; data.position >>= 1;
      if (data.position === 0) { data.position = resetValue; data.val = getNextValue(data.index++); }
      bits |= (resb > 0 ? 1 : 0) * power; power <<= 1;
    }
    c = bits;
    switch (c) {
      case 0:
        bits = 0; maxpower = Math.pow(2, 8); power = 1;
        while (power !== maxpower) {
          resb = data.val & data.position; data.position >>= 1;
          if (data.position === 0) { data.position = resetValue; data.val = getNextValue(data.index++); }
          bits |= (resb > 0 ? 1 : 0) * power; power <<= 1;
        }
        dictionary[dictSize++] = String.fromCharCode(bits);
        c = dictSize - 1; enlargeIn--; break;
      case 1:
        bits = 0; maxpower = Math.pow(2, 16); power = 1;
        while (power !== maxpower) {
          resb = data.val & data.position; data.position >>= 1;
          if (data.position === 0) { data.position = resetValue; data.val = getNextValue(data.index++); }
          bits |= (resb > 0 ? 1 : 0) * power; power <<= 1;
        }
        dictionary[dictSize++] = String.fromCharCode(bits);
        c = dictSize - 1; enlargeIn--; break;
      case 2: return result;
    }
    if (enlargeIn === 0) { enlargeIn = Math.pow(2, numBits); numBits++; }
    if (dictionary[c]) { entry = dictionary[c]; }
    else if (c === dictSize) { entry = w + w.charAt(0); }
    else { return ""; }
    result += entry;
    dictionary[dictSize++] = w + entry.charAt(0);
    enlargeIn--;
    w = entry;
    if (dictSize === Math.pow(2, numBits)) { numBits++; }
  }
}
module.exports = { decompressFromBase64 };
