# Feature Gate Multisig Tool

A command-line tool for rapidly provisioning minimal Squads multisig setups specifically designed for Solana feature gate governance. This tool enables parties to create multisig wallets where the default vault address can be mapped to feature gate account addresses, allowing collective voting on whether new Solana features should be implemented.

## 🎯 Purpose

This tool is designed to streamline the creation of governance multisigs for Solana feature gates. Feature gates are mechanisms that control the activation of new blockchain features, and this tool enables decentralized governance by allowing multiple parties to collectively vote on feature implementations through a multisig structure.

## ✨ Key Features

- **🚀 Rapid Provisioning**: Quickly create Squads multisig wallets with minimal configuration
- **🌐 Multi-Network Deployment**: Deploy the same configuration across multiple Solana networks with automatic or manual deployment modes
- **👥 Member Management**: Interactive collection of member public keys with automatic permission assignment
- **📋 Persistent Configuration**: Same create key across deployments ensures consistent addresses
- **🎨 Rich CLI Experience**: Colored output and professional table formatting
- **📊 Comprehensive Reporting**: Detailed deployment summary with all addresses and transaction signatures

## 🛠 Installation

```bash
# Clone the repository
git clone <repository-url>
cd feature-gate-multisig

# Build the project
cargo build --release

# The binary will be available at ./target/release/feature-gate-multisig-tool
```

## 📖 Usage

### Command Line Arguments

```bash
feature-gate-multisig-tool [COMMAND]
```

### Commands

| Command | Description |
|---------|-------------|
| `create` | Create a new multisig wallet |
| `show <ADDRESS>` | Show feature multisig details for a given address |
| `interactive` | Start interactive mode (default when no command specified) |
| `config` | Show current configuration including networks array |

### Create Command Options

```bash
feature-gate-multisig-tool create [OPTIONS]

Options:
  -t, --threshold <THRESHOLD>    Number of required signatures
  -s, --signers <SIGNERS>       Signers (currently unused in favor of interactive collection)
  -h, --help                    Print help information
```

### Config Command Example

View your current configuration including the networks array:

```bash
feature-gate-multisig-tool config
```

**Output:**
```
📋 Configuration:
  Config file: "/Users/user/.feature-gate-multisig-tool/config.json"
  Default threshold: 2
  Saved networks: 3 networks
    Network 1: https://api.devnet.solana.com
    Network 2: https://api.testnet.solana.com
    Network 3: https://api.mainnet-beta.solana.com
  Saved members: 2 members
    Member 1: 9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM
    Member 2: 7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU
```

## 🎮 Interactive Mode

The tool is designed to work primarily in interactive mode, providing a guided experience:

### 1. **Configuration Setup**
- **Config Review**: Checks for existing saved configuration and asks for confirmation
- **Member Loading**: If config exists, loads saved members with full permissions
- **Interactive Fallback**: If no config or user declines, collects members interactively
- **Contributor Generation**: Always generates fresh ephemeral contributor keypair (never saved)
- **Create Key**: Creates persistent key for consistent addresses across networks

### 2. **Member Collection**
- Contributor is automatically added with Initiate-only permissions (bitmask 1)
- Interactive prompts to add additional members
- Additional members receive full permissions with bitmask 7 (Initiate | Vote | Execute)
- Real-time validation of public key formats

### 3. **Multi-Network Deployment**
The tool supports two deployment modes:

**🔄 Automatic Deployment Mode**
- Configure multiple networks in your config file using the `networks` array
- Deploy to all saved networks automatically with a single confirmation
- Consistent addresses across all networks using the same create key

**⚙️ Manual Deployment Mode**
- Enter RPC endpoints one by one during deployment
- Choose to continue or stop after each deployment
- Flexible for ad-hoc deployments to custom networks

**📦 Pre-deployment Preview**
For each deployment, the tool shows:
- Create key and contributor key
- Derived multisig PDA and vault PDA (index 0)  
- All member keys with their permissions

### 4. **Deployment Summary**
Professional summary tables showing:
- Configuration details (create key, threshold, members)
- Members & permissions table
- Network deployments with addresses
- Transaction signatures for each deployment

## 🏗 How It Works

### Multisig Creation Process

1. **Key Generation**: Creates persistent create key and contributor keypair
2. **Member Setup**: Collects member public keys interactively
3. **Address Derivation**: Calculates multisig and vault PDAs using Squads program
4. **Transaction Building**: Constructs `MultisigCreateV2` instruction with:
   - 8-byte discriminator
   - Borsh-serialized arguments
   - Proper account metadata
5. **Multi-Network Support**: Deploys identical configuration across different networks
6. **Confirmation**: Provides comprehensive deployment summary

### Key Components

- **Create Key**: Persistent across deployments, ensures consistent addresses
- **Multisig PDA**: Derived from create key using Squads program seeds
- **Vault PDA**: Default vault (index 0) that can be mapped to feature gate addresses
- **Members**: Contributor receives Initiate-only permissions; additional members receive full permissions for governance participation

## 🎯 Feature Gate Integration

The primary goal is to create multisig structures for Solana feature gate governance:

### Feature Gate Mapping
- The **default vault address** (index 0) serves as the governance account
- This vault can be mapped to specific feature gate account addresses
- Enables decentralized voting on feature activation/deactivation

### Governance Workflow
1. **Multisig Creation**: Use this tool to create governance multisig
2. **Feature Gate Mapping**: Map vault address to feature gate account
3. **Proposal Creation**: Members can initiate proposals for feature changes
4. **Voting Process**: Members vote using their multisig permissions
5. **Execution**: Approved changes are executed through the multisig

## 🌐 Network Support

Supports deployment to any Solana network:
- **Mainnet Beta**: `https://api.mainnet-beta.solana.com`
- **Devnet**: `https://api.devnet.solana.com` (default)
- **Testnet**: `https://api.testnet.solana.com`
- **Custom RPC**: Any valid Solana RPC endpoint

## 📋 Configuration

The tool maintains configuration in `~/.feature-gate-multisig-tool/config.json`:

### Single Network Configuration (Legacy)
```json
{
  "threshold": 2,
  "members": [
    "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
    "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU"
  ],
  "network": "https://api.devnet.solana.com"
}
```

### Multi-Network Configuration (Recommended)
```json
{
  "threshold": 2,
  "members": [
    "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
    "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
    "4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ"
  ],
  "networks": [
    "https://api.devnet.solana.com",
    "https://api.testnet.solana.com",
    "https://api.mainnet-beta.solana.com"
  ],
  "network": "https://api.devnet.solana.com"
}
```

### Configuration Features

- **Automatic Saving**: After successful deployments, member lists and threshold are saved
- **Config Review**: On startup, shows existing configuration and asks if you want to use it
- **Member Preloading**: Saved members are automatically loaded with full permissions (Initiate, Vote, Execute)
- **Network Array Support**: Configure multiple networks for automatic deployment using the `networks` array
- **Deployment Mode Selection**: Choose between automatic deployment to saved networks or manual entry
- **Contributor Exclusion**: The ephemeral contributor key is never saved to config
- **Backward Compatibility**: Supports legacy single `network` field alongside new `networks` array

### Config Example

A `config.example.json` file is provided showing the expected format:

```json
{
  "threshold": 2,
  "members": [
    "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
    "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
    "4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ"
  ],
  "network": "https://api.devnet.solana.com"
}
```

## 🔧 Technical Details

### Dependencies
- **Solana SDK**: Blockchain interaction
- **Squads Protocol**: Multisig program integration
- **Borsh**: Serialization
- **Colored**: Terminal output formatting
- **Tabled**: Professional table formatting
- **Inquire**: Interactive prompts
- **Dialoguer**: Confirmation dialogs

### Key Addresses Generated
- **Multisig PDA**: Main multisig account
- **Vault PDA (index 0)**: Default vault for feature gate mapping
- **Program Config**: Squads program configuration account

## 🚨 Important Notes

- **Persistent Keys**: The same create key is used across all deployments
- **Permission Model**: Contributor has Initiate-only (bitmask 1); additional members have full permissions (bitmask 7)
- **Network Consistency**: Identical addresses across different networks
- **Transaction Fees**: Requires SOL for transaction fees on target networks
- **Security**: Generated keys are ephemeral - save important addresses from output

## 📊 Example Output

### Automatic Deployment to Multiple Networks

```
🚀 Creating feature gate multisig configuration

📋 Found existing configuration:
  Threshold: 2
  Saved networks: 3 networks
    Network 1: https://api.devnet.solana.com
    Network 2: https://api.testnet.solana.com
    Network 3: https://api.mainnet-beta.solana.com
  Saved members: 2 members
    Member 1: 9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM
    Member 2: 7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU

✅ Use these saved members and settings? Yes

🔄 Deploy to all saved networks automatically? Yes

🌐 Automatic deployment mode - deploying to 3 networks

✅ Using saved configuration

📋 Final Configuration:
  Contributor public key: 4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ
  Create key: 8xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU
  Threshold: 2

👥 All Members:
  ✓ Contributor: 4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ (Initiate)
  ✓ Member 1: 9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM (Initiate, Vote, Execute)
  ✓ Member 2: 7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU (Initiate, Vote, Execute)

🎉 DEPLOYMENT SUMMARY
════════════════════════════════════════════════════════════════════════════════

📋 Configuration:
  Create Key: 4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ
  Threshold: 2
  Total Members: 2

👥 Members & Permissions:
╭─────┬────────────────────────────────────────────────┬─────────────────────────╮
│ #   │ Public Key                                     │ Permissions             │
├─────┼────────────────────────────────────────────────┼─────────────────────────┤
│ 1   │ 7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU (Contributor) │ Initiate               │
│ 2   │ 9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM   │ Initiate, Vote, Execute │
╰─────┴────────────────────────────────────────────────┴─────────────────────────╯

🌐 Network Deployments:
╭─────┬───────────────────────────────────────┬──────────────────────────────────────────────────┬──────────────────────────────────────────────────┬─────────────────────────────────────────────╮
│ #   │ Network                               │ Multisig Address                                 │ Vault Address                                    │ Transaction Signature                       │
├─────┼───────────────────────────────────────┼──────────────────────────────────────────────────┼──────────────────────────────────────────────────┼─────────────────────────────────────────────┤
│ 1   │ https://api.devnet.solana.com         │ 8xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU     │ 4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ     │ 5J7...xyz                                   │
│ 2   │ https://api.testnet.solana.com        │ 8xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU     │ 4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ     │ 9B2...abc                                   │
│ 3   │ https://api.mainnet-beta.solana.com   │ 8xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU     │ 4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ     │ 3F8...def                                   │
╰─────┴───────────────────────────────────────┴──────────────────────────────────────────────────┴──────────────────────────────────────────────────┴─────────────────────────────────────────────╯

✅ Successfully deployed to 3 network(s)!

💾 Configuration saved for future use
```

### Manual Network Entry Mode

```
🚀 Creating feature gate multisig configuration

🔄 Manual network entry mode

Enter RPC URL for deployment: https://api.devnet.solana.com

🌐 Deploying to: https://api.devnet.solana.com
📦 All public keys for this deployment:
  Create key: 8xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU
  Contributor: 4Qkev8aNZcqFNSRhQzwyLMFSsi94jHqE8WNVTJzTP6kQ
  Multisig PDA: 7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU
  Vault PDA (index 0): 9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM

✅ Deployment successful on https://api.devnet.solana.com

Deploy to another network with the same configuration? No
```

## 🤝 Contributing

Contributions are welcome! Please ensure:
- Code follows Rust conventions
- All tests pass
- Documentation is updated for new features
- Security best practices are maintained

## 📄 License

[Add your license information here]

## 🆘 Support

For support, please:
1. Check existing issues in the repository
2. Create a new issue with detailed information
3. Include relevant logs and configuration details