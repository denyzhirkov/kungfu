terraform {
  required_version = ">= 1.5"
  required_providers {
    aws = { source = "hashicorp/aws" }
  }
}

provider "aws" {
  region = var.region
  alias  = "primary"
}

locals {
  name_prefix = "acme-${var.env}"
  common_tags = {
    Env   = var.env
    Owner = "platform"
  }
}

resource "aws_s3_bucket" "logs" {
  bucket = "acme-${var.env}-logs"
  tags   = {
    Name = "logs {primary}" # a brace } in a string and a comment {
    Env  = var.env
  }

  policy = <<-POLICY
    {
      "Version": "2012-10-17",
      "Statement": [{ "Effect": "Allow", "Action": "s3:*" }]
    }
  POLICY

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_security_group" "web" {
  name        = "web"
  description = "Web ingress"

  ingress {
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  ingress {
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    from_port = 0
    to_port   = 0
    protocol  = "-1"
  }
}

data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"]
}

module "vpc" {
  source = "terraform-aws-modules/vpc/aws"
  cidr   = "10.0.0.0/16"
}

output "bucket_arn" {
  value = aws_s3_bucket.logs.arn
}
