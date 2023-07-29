const path = require('path');

module.exports = {
  mode: 'production',
  entry: {
    client: './src/client.js',
  },
  output: {
    path: path.resolve(__dirname, 'dist'), // eslint-disable-line
    libraryTarget: 'commonjs',
    filename: '[name].bundle.js',
  },
  module: {
    rules: [{ test: /\.js$/, use: 'babel-loader' }],
  },
  target: 'node18',
};