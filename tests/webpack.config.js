const path = require('path');

module.exports = {
  mode: 'production',
  entry: {
    client: './src/load/client.js',
  },
  output: {
    path: path.resolve(__dirname, 'dist', 'load'), // eslint-disable-line
    libraryTarget: 'commonjs',
    filename: '[name].bundle.js',
  },
  module: {
    rules: [{ test: /\.js$/, use: 'babel-loader' }],
  },
  target: 'node18',
};