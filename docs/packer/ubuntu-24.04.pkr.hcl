// Template Ubuntu 24.04 LTS (Noble) pour l'API : packer_config d'un template, avec
// disk_name = "ubuntu-24.04.qcow2".
// Compte : ubuntu / ubuntu (sudo). Part de l'image cloud officielle d'Ubuntu.

packer {
  required_plugins {
    qemu = {
      source  = "github.com/hashicorp/qemu"
      version = ">= 1.1.0"
    }
  }
}

source "qemu" "ubuntu" {
  iso_url          = "https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img"
  iso_checksum     = "file:https://cloud-images.ubuntu.com/noble/current/SHA256SUMS"
  disk_image       = true
  disk_size        = "20G"
  format           = "qcow2"
  disk_compression = true
  vm_name          = "ubuntu-24.04.qcow2"
  output_directory = "output"
  headless         = true
  accelerator      = "kvm"
  cpus             = 2
  memory           = 2048

  // cloud-init ne sert que pendant le build : il crée le compte ubuntu/ubuntu et agrandit
  // la partition racine à disk_size.
  cd_label = "cidata"
  cd_content = {
    "meta-data" = "instance-id: packer\nlocal-hostname: ubuntu\n"
    "user-data" = <<-EOF
      #cloud-config
      ssh_pwauth: true
      users:
        - name: ubuntu
          plain_text_passwd: ubuntu
          lock_passwd: false
          groups: [adm, sudo]
          sudo: ALL=(ALL) NOPASSWD:ALL
          shell: /bin/bash
    EOF
  }

  communicator     = "ssh"
  ssh_username     = "ubuntu"
  ssh_password     = "ubuntu"
  ssh_timeout      = "15m"
  shutdown_command = "sudo shutdown -P now"
}

build {
  sources = ["source.qemu.ubuntu"]

  provisioner "shell" {
    inline = [
      "cloud-init status --wait || true",
      "sudo apt-get update",
      "sudo DEBIAN_FRONTEND=noninteractive apt-get -y upgrade",
      "sudo DEBIAN_FRONTEND=noninteractive apt-get -y install qemu-guest-agent",
      "sudo systemctl enable qemu-guest-agent",

      // Les VM créées depuis ce template n'ont pas de cloud-init (pas de disque cidata) :
      // on le désactive et on fige ce qu'il aurait fait à chaque démarrage.
      "sudo touch /etc/cloud/cloud-init.disabled",

      // Réseau : DHCP sur toutes les cartes, quelle que soit leur adresse MAC (celle de
      // cloud-init visait la MAC de la VM de build). optional : une carte sans DHCP (réseau
      // interne d'un lab) ne bloque pas le démarrage.
      "sudo rm -f /etc/netplan/50-cloud-init.yaml",
      "printf 'network:\\n  version: 2\\n  ethernets:\\n    all-en:\\n      match:\\n        name: \"en*\"\\n      dhcp4: true\\n      dhcp-identifier: mac\\n      optional: true\\n' | sudo tee /etc/netplan/01-dhcp.yaml",
      "sudo chmod 600 /etc/netplan/01-dhcp.yaml",

      // Connexion SSH par mot de passe.
      "echo 'PasswordAuthentication yes' | sudo tee /etc/ssh/sshd_config.d/10-password.conf",

      // Chaque VM régénère ses clés SSH d'hôte et son machine-id au premier démarrage.
      "sudo mkdir -p /etc/systemd/system/ssh.service.d",
      "printf '[Service]\\nExecStartPre=\\nExecStartPre=/usr/bin/ssh-keygen -A\\nExecStartPre=/usr/sbin/sshd -t\\n' | sudo tee /etc/systemd/system/ssh.service.d/10-host-keys.conf",
      "sudo rm -f /etc/ssh/ssh_host_*",
      "sudo truncate -s 0 /etc/machine-id",
      "sudo rm -f /var/lib/dbus/machine-id",

      // Ménage.
      "sudo apt-get -y autoremove --purge",
      "sudo apt-get clean",
      "sudo cloud-init clean --logs --seed",
      "sudo rm -rf /tmp/* /var/tmp/*",
      "sudo fstrim -av || true",
    ]
  }
}
